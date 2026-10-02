// SPDX-License-Identifier: AGPL-3.0-or-later
//! Daemon-backed Runtime bootstrap for `svrn chat`.
//!
//! Mirrors `sovereign-desktop::state::bootstrap` — same StateStore,
//! CorpusEngine, tools, mesh-knowledge wiring — but the
//! `InferenceProvider` is a `SplitInferenceProvider` that delegates
//! chat completions to the daemon's chat model and embeddings to the
//! daemon's embed model over HTTP. No embedded llama.cpp, no Tauri.
//!
//! Rationale
//! ---------
//! The desktop's Attach mode is the architectural template we want:
//! "the daemon already owns the model, talk to it over HTTP". The
//! desktop currently still loads local weights even in Attach mode
//! (historical quirk); this CLI does what Attach *should* do — pure
//! HTTP.
//!
//! The split-provider dance is required because `RemoteApiProvider`
//! uses a single `model_id` for both `/chat/completions` AND
//! `/embeddings`. Sending a chat model to the embeddings endpoint
//! returns non-embedding shapes (or errors). We keep two instances
//! and route by method.

use std::path::PathBuf;
use std::sync::Arc;

use sovereign_core::conv_tiered::ConvTieredReader;
use sovereign_core::error::{Error, Result};
use sovereign_core::runtime::Runtime;
use sovereign_core::traits::{ApprovalChannel, InferenceProvider, StateStore};
use sovereign_core::types::*;
use sovereign_core::SkillRegistry;
use sovereign_runtime_recipe::{LaneScope, LaneWarmth, RecipeInputs, RecipeProgress};
// Re-exported (not just `use`d) so the other CLI modules that referenced the
// formerly-local `chat_cmd::bootstrap::SplitInferenceProvider` (raptor,
// recipe_cmd) keep resolving after it was promoted to sovereign-inference.
pub use oicp_client::SplitInferenceProvider;
use sovereign_store::sqlite::SqliteStateStore;

use crate::chat_cmd::config::ChatGlobals;

/// Bundle of everything the chat subcommands need from bootstrap.
/// Carries `Arc<Runtime>` plus the handles required to persist turns
/// (the store) and browse prior conversations.
pub struct ChatSession {
    pub runtime: Arc<Runtime>,
    pub store: Arc<dyn StateStore>,
    /// Ingest's read port over the engine the composed ingest built; `None`
    /// in the bare binary (read it through [`ChatSession::corpus`]).
    pub corpus_engine: Option<Arc<dyn corpus_index::source::CorpusReadPort>>,
    pub inference: Arc<dyn InferenceProvider>,
    pub daemon_base: String,
    /// Resolved embed model id (e.g. `Qwen3-Embedding-0.6B-Q8_0`).
    /// Surfaced so cache layers (atlas embeddings, future per-corpus
    /// vector caches) can key on the active model and invalidate when
    /// the operator swaps it.
    pub embed_model: String,
    /// The per-process atlas-grounding manager (the same Arc installed
    /// on `runtime` as its `AtlasContextProvider`). Exposed so a
    /// measurement harness can `warm_one(corpus)` its sealed corpus —
    /// `build_session` only loads already-cached atlases
    /// (`init_from_cache`), so a freshly-enriched corpus contributes 0
    /// contexts until something warms it. Warming this Arc is visible to
    /// `runtime` because they share it.
    pub atlas_mgr: Arc<sovereign_tools::atlas_context_manager::AtlasContextManager>,
}

impl ChatSession {
    /// The corpus read port, or the named absence when no ingest program is
    /// composed in this process (pb-cli-llm-ingest-move-compose).
    pub fn corpus(
        &self,
    ) -> std::result::Result<&Arc<dyn corpus_index::source::CorpusReadPort>, &'static str> {
        self.corpus_engine.as_ref().ok_or(super::ingest::NO_INGEST)
    }
}

/// Probe the daemon, resolve its `(chat, embed)` model ids, and build the
/// HTTP `InferenceProvider` over them. Returns `(inference, daemon_base,
/// embed_model_id)`.
///
/// # Why this is its own function
///
/// It is the first two steps of [`build_session_with_skills`], and for one
/// caller it is ALL of them. `svrn atlas backfill-ann` needs an embedder and
/// an atlas directory; it was reaching them through `build_session`, which
/// also opens the state store, builds a `CorpusEngine`, and commissions the
/// shared recipe — and the recipe loads the wiki graph (51,280 articles, 7.3M
/// edges) and the meta-atlas (1.57M atoms) into the CLI process beside the
/// resident daemon. On 2026-09-04 two concurrent `backfill-ann` invocations
/// OOM-killed the daemon at 11:52:08. The embeds were never the price; the
/// bootstrap was.
///
/// So the cheap half is named, and the expensive half is what a caller opts
/// INTO by asking for a `ChatSession`. `build_session_with_skills` calls this
/// rather than keeping a second copy of it (ARCH §10.6).
pub async fn build_inference(
    globals: &ChatGlobals,
) -> Result<(Arc<dyn InferenceProvider>, String, String)> {
    oicp_client::daemon_inference::build_inference(
        &globals.daemon_base,
        globals.bearer.as_deref(),
        globals.chat_model.as_deref(),
        globals.embed_model.as_deref(),
        globals.guest_link_active,
        globals.guest_lender_url.as_deref(),
    )
    .await
}

/// Build a `Runtime` backed by the daemon over HTTP.
///
/// Fails fast if the daemon isn't answering — there's no recovery
/// path a retry could fix, and a partially-initialized Runtime
/// pointing at a dead endpoint would produce confusing errors deep
/// in retrieval. The caller should exit with a hint.
pub async fn build_session(globals: &ChatGlobals) -> Result<ChatSession> {
    build_session_with_skills(globals, SkillRegistry::new()).await
}

/// Build a `ChatSession` SEALED to one corpus.
///
/// For a surface that has already resolved the single corpus it will ask
/// about — a bench lane, a one-shot eval. The cross-corpus enrichment lane
/// members (the wikipedia link graph, the meta-atlas, the bridge index) are
/// then unreachable by construction, so [`LaneScope::Sealed`] does not load
/// them. See that enum for why this is a no-op rather than a degradation.
///
/// Measured on the authoring host (`svrn bench chaos-monkey run --corpus
/// chaos-secret-agent`, 316 chunks): startup 24.9 s → 2.6 s, because 22.3 s
/// of it was a 7.85M-edge graph and a 981 MB meta-atlas JSON that a lane
/// sealed to a 316-chunk corpus can never consult. `svrn quality check`
/// pays that startup once per in-process bench lane, not once per run.
pub async fn build_session_sealed(globals: &ChatGlobals, corpus_id: &str) -> Result<ChatSession> {
    build_session_scoped(
        globals,
        SkillRegistry::new(),
        LaneScope::Sealed(corpus_id.to_string()),
    )
    .await
}

/// Build a daemon-backed `ChatSession` with a caller-supplied
/// `SkillRegistry`. The default `build_session` passes an empty
/// registry — chat-as-chat doesn't need skills loaded. The Tier-B
/// voice eval harness (`svrn voice eval`) supplies a registry
/// pre-populated with the relational skills (inner-work,
/// personal-assistant) and pre-activates the per-scenario one so
/// the runtime's `primary_skill_register()` resolves to
/// `Relational` and the witness-voice contract gets prepended.
pub async fn build_session_with_skills(
    globals: &ChatGlobals,
    skills: SkillRegistry,
) -> Result<ChatSession> {
    build_session_scoped(globals, skills, LaneScope::All).await
}

/// The one body. `scope` is the host input every entry point above resolves
/// — `All` for a general-purpose `svrn chat`, `Sealed` for a lane that has
/// already picked its corpus (ARCH §10.6: one decider).
async fn build_session_scoped(
    globals: &ChatGlobals,
    skills: SkillRegistry,
    scope: LaneScope,
) -> Result<ChatSession> {
    // Steps 1 and 2 — probe the daemon, resolve the model ids, build the HTTP
    // provider — are `build_inference` above. They are the whole of what
    // `svrn atlas backfill-ann` needs, and everything below this line (state
    // store, CorpusEngine, the recipe's wiki graph and meta-atlas) is what
    // made that verb cost ~20 GB of RSS beside the resident daemon. One
    // implementation, two callers (ARCH §10.6) — ei-3b step 0.
    let (inference, base, embed_model) = build_inference(globals).await?;

    // 3. Open the state store. Creating the data dir on the fly is
    //    safe — mirrors the desktop's behaviour and means a first
    //    `svrn chat` against a fresh home directory doesn't
    //    stumble on a missing folder.
    std::fs::create_dir_all(&globals.data_dir)
        .map_err(|e| Error::Serialization(format!("create {:?}: {e}", globals.data_dir)))?;
    let db_path = globals.data_dir.join("sovereign.db");
    eprintln!("Database:    {}", db_path.display());
    let store_concrete = Arc::new(
        SqliteStateStore::open(&db_path)
            .map_err(|e| Error::Serialization(format!("open db {:?}: {e}", db_path)))?,
    );
    let store: Arc<dyn StateStore> = store_concrete.clone();

    // 4. Ingest's engine, built by the composed ingest program. The desktop (`state.rs`) hardcodes
    //    `~/.svrnmesh/{recipes,indexes}` regardless of `config.data.dir` —
    //    that field governs the state DB only, not corpus storage. Matching
    //    that convention means this CLI sees the same corpora the desktop
    //    just ingested.
    //
    //    If a user passed `--data-dir` explicitly they almost
    //    certainly meant to override BOTH paths; honour that by
    //    using `<data_dir>/indexes` when `--data-dir` was given.
    //    Otherwise stick to the hardcoded well-known path.
    let dotsovereign = sovereign_contracts::rebrand::svrnmesh_root();
    //    Ingest roots `recipes/` and `indexes/` under the one directory.
    let ingest_root: PathBuf = if globals.data_dir_explicit {
        globals.data_dir.clone()
    } else {
        dotsovereign
    };
    let indexes_dir = ingest_root.join("indexes");
    eprintln!("Indexes:     {}", indexes_dir.display());
    // The engine's `expected_embedding_model` flows into
    // `_corpus_meta.json` at ingest time and into shard-consistency
    // checks. The CLI doesn't ingest during chat, but if any tool
    // path later triggers an ingest (e.g. watcher-driven reindex
    // through the same engine), it must match what the desktop
    // would have written.
    let mount = super::ingest::compose(
        ingest_root,
        Arc::clone(&inference),
        &embed_model,
        Arc::clone(&store_concrete),
    );
    if mount.is_none() {
        eprintln!("Ingest:      absent — {}", super::ingest::NO_INGEST);
    }
    let ingest_port = mount.map(|m| m.port);
    let ingest_ports = super::ingest::ports();

    // 5. Session-level overrides. `govern ask` sets custom instructions to its
    //    governance answering rules; ordinary chat leaves them None
    //    (byte-identical prompt to before).
    let mut inference_config = InferenceConfig::default();
    if let Some(t) = globals.temperature {
        inference_config.temperature = t;
        eprintln!("Temperature: {t} (override)");
    }
    if let Some(n) = globals.max_tokens {
        inference_config.max_tokens = n;
        eprintln!("Max tokens: {n} (override)");
    }
    if globals.custom_instructions.is_some() {
        inference_config.custom_instructions = globals.custom_instructions.clone();
    }

    // 6. Tool-Mastery Layer 3 — svrn's memory notes for the per-conversation
    //    `tool_decision` write hook, lessons and commitments: the store opened
    //    above, the same one the daemon keeps them in (pb-notes-memory), so
    //    the chat REPL and bench surfaces share one outcome log.
    let note_store: Option<Arc<dyn sovereign_contracts::notes::AgentNotes>> =
        Some(store_concrete.clone());

    // ── The shared recipe ────────────────────────────────────────────────
    //
    // Tools, the router classifier stack, the planner and the enrichment lane
    // used to be ~450 lines right here, and the desktop and the server each
    // had their own copy. `sovereign-runtime-recipe` is the one of them
    // (TOPOLOGY.md §10 phase 5c); what stays above is only what a host is the
    // sole thing able to answer — which daemon to talk to, which models it
    // loaded, where this invocation's data root is.
    let skills = Arc::new(skills);
    let common = sovereign_runtime_recipe::common_parts(
        RecipeInputs {
            inference: Arc::clone(&inference),
            store: Arc::clone(&store),
            // The same `SqliteStateStore` handle already opened above also
            // impls `ConvTieredReader` (spec CONV_TIERED_PORT.md).
            conv_tiered: Some(Arc::clone(&store_concrete) as Arc<dyn ConvTieredReader>),
            corpus_engine: ingest_port.clone().map(|port| port as _),
            atlas: ingest_ports.map(|ingest| ingest.atlas()),
            // The composed ingest's catalog, so the atlas manager's
            // pipeline-map fallback reads configs as it did before the port.
            enrich_config: ingest_ports.map(|ingest| ingest.enrich_config()),
            // Cloned: `tool_bundles` below borrows the same handle for
            // `knowledge_lookup`'s notes channel. One store, two readers.
            note_store: note_store.clone(),
            skills: Arc::clone(&skills),
            // Chat turns don't trigger confirmations in the normal path; a
            // yes-only stub keeps a stray approval request from deadlocking a
            // one-shot CLI.
            // Core's `AutoApprovalChannel`, not a local twin of it. The
            // local one differed by exactly one line — `ask_user` returned
            // `Ok("")`, an invented reply the executor cannot tell from a
            // real one (ARCH §18.3). Core's refuses by name.
            approval: Arc::new(sovereign_core::executor::AutoApprovalChannel)
                as Arc<dyn ApprovalChannel>,
            inference_config,
            indexes_dir: indexes_dir.clone(),
            embed_model: embed_model.clone(),
            // The families this surface's turn registry carries. Shell is
            // present because an interactive CLI runs as the invoking user,
            // in the directory they invoked it from, for the length of one
            // command they are watching.
            tool_bundles: {
                let mut b = sovereign_runtime_recipe::baseline_bundles(
                    sovereign_runtime_recipe::BaselineDeps {
                        store: &store,
                        inference: &inference,
                        corpus_engine: ingest_port.clone().map(|port| port as _),
                        // The same handle passed to `note_store` below — the
                        // notes evidence channel and the tool-decision write
                        // hook read one store, not two.
                        note_store: note_store.as_ref(),
                        web: sovereign_tools::bundles::WebReach::Granted(
                            sovereign_core::egress::search_client()
                                .expect("egress boundary search client build"),
                        ),
                        // `svrn chat` has no operator switch for it; the
                        // user-in-loop escalation card is still there.
                        escalation: sovereign_tools::bundles::WebEscalation::Disabled,
                    },
                );
                b.push(match (&ingest_port, ingest_ports) {
                    (Some(port), Some(ingest)) => {
                        Box::new(sovereign_tools::bundles::WikipediaTools::new(
                            Arc::clone(port) as _,
                            ingest.atlas(),
                        ))
                    }
                    _ => Box::new(sovereign_contracts::tool_bundle::Withheld::new(
                        "wikipedia",
                        "no ingest program is composed in this process, and \
                         wikipedia_fetch reads its catalog corpus",
                    )),
                });
                b.push(Box::new(sovereign_tools::bundles::ShellTools));
                b
            },
            // No settings panel on this host, so nothing to consult: every
            // family composed above registers.
            switches: sovereign_runtime_recipe::ToolSwitches::Ungoverned,
            // No config file of its own: the canonical `[[mcp_servers]]` array
            // is the whole declaration on this host.
            mcp_extra: Vec::new(),
            // A one-shot answering one question would rather wait once than
            // answer it with less. Byte-identical to the behaviour this
            // function had when the recipe was inline.
            warmth: LaneWarmth::Eager,
            // Resolved by the entry point: `All` for chat-as-chat, and
            // `Sealed` for a bench lane that already knows its one
            // corpus. See `build_session_sealed` for the measurement.
            scope,
            // serve's cross-encoder and NER model: this process loads no
            // model (pb-cli-llm).
            rerank: serve_reranker(&Banner).await,
            ner: serve_ner(&Banner).await,
        },
        &Banner,
    )
    .await;

    // Mesh knowledge client. Talks to the daemon's `/v1/mesh` — when no mesh
    // is running, reqwest gets ECONNREFUSED on the first call and retrieval
    // falls through to local-only. Safe to install unconditionally (same
    // policy as the desktop). A host input, not part of the recipe: inside the
    // daemon this is a loopback call to itself and dissolves (§3.5).
    let mesh_knowledge: Option<Arc<dyn sovereign_core::traits::MeshKnowledgeSource>> =
        match sovereign_turn_client::knowledge_client::MeshKnowledgeClient::new(&base) {
            Ok(c) => Some(Arc::new(c)),
            Err(_) => None,
        };

    // Named absences, written as a diff against the recipe's baseline. `svrn
    // chat` genuinely has no compaction worker, no landscape-digest provider,
    // no sensitivity oracle, no principal resolver and no folder-metadata
    // oracle. Narration is a real gap here (the CLI renders none); it is
    // written down rather than being invisible, which is what it was for the
    // whole life of the builder surface.
    let runtime = sovereign_runtime_recipe::commission(sovereign_core::RuntimeParts {
        mesh_knowledge,
        routing_events: Arc::new(sovereign_core::traits::NoOpRoutingEventSink),
        ..common.parts
    });

    Ok(ChatSession {
        runtime,
        store,
        corpus_engine: ingest_port.map(|port| port as _),
        inference,
        daemon_base: base,
        embed_model,
        atlas_mgr: common.atlas_context,
    })
}

/// The CLI's boot banner. `svrn chat` prints the recipe's progress lines to
/// stderr, one per line, exactly as it did when the recipe was inline here —
/// a daemon commissioning the same `Runtime` traces them instead.
struct Banner;

/// serve's cross-encoder, when serve holds one (`SOVEREIGN_RERANK_MODEL_PATH`
/// takes effect at serve's start). A serve that did not answer is printed and
/// the turn runs without one; the dedup-only ablation takes precedence, so
/// under it serve is not asked.
async fn serve_reranker(progress: &dyn RecipeProgress) -> Option<Arc<dyn InferenceProvider>> {
    if sovereign_runtime_recipe::rerank_dedup_only() {
        return None;
    }
    crate::serve_dial::serve_reranker("svrn chat")
        .await
        .unwrap_or_else(|e| {
            progress.note(&format!("Reranker:    none — {e}"));
            None
        })
}

/// serve's NER model, when serve holds one. A serve that did not answer is
/// printed and the turn runs without an entity extractor.
async fn serve_ner(
    progress: &dyn RecipeProgress,
) -> Option<Arc<dyn sovereign_contracts::ner::LabeledEntityExtractor>> {
    crate::serve_dial::serve_ner("svrn chat")
        .await
        .unwrap_or_else(|e| {
            progress.note(&format!("NER:         none — {e}"));
            None
        })
}

impl RecipeProgress for Banner {
    fn note(&self, line: &str) {
        eprintln!("{line}");
    }
}
