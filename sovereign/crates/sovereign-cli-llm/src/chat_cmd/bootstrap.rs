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
use std::time::Duration;

use async_trait::async_trait;

use sovereign_core::conv_tiered::ConvTieredReader;
use sovereign_core::error::{Error, Result};
use sovereign_core::runtime::Runtime;
use sovereign_core::traits::{ApprovalChannel, InferenceProvider, StateStore};
use sovereign_core::types::*;
use sovereign_core::SkillRegistry;
use sovereign_runtime_recipe::{LaneScope, LaneWarmth, RecipeInputs, RecipeProgress, RerankWiring};
// Re-exported (not just `use`d) so the other CLI modules that referenced the
// formerly-local `chat_cmd::bootstrap::SplitInferenceProvider` (raptor,
// recipe_cmd) keep resolving after it was promoted to sovereign-inference.
pub use sovereign_inference::remote::SplitInferenceProvider;
use sovereign_store::sqlite::SqliteStateStore;

use crate::chat_cmd::config::ChatGlobals;

/// Bundle of everything the chat subcommands need from bootstrap.
/// Carries `Arc<Runtime>` plus the handles required to persist turns
/// (the store) and browse prior conversations.
pub struct ChatSession {
    pub runtime: Arc<Runtime>,
    pub store: Arc<dyn StateStore>,
    pub corpus_engine: Arc<corpus_engine::CorpusEngine>,
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
    // 1. Probe the daemon before we touch anything else. A fast fail
    //    here prints a clean "start the daemon" message instead of
    //    the cryptic timeout from the first real request.
    let base = globals.daemon_base.clone();
    let v1 = format!("{base}/v1");
    probe_or_bail(&base, globals.bearer.as_deref()).await?;

    // 2. Resolve model IDs. Preference order:
    //       a) explicit `--chat-model` / `--embed-model` flag,
    //       b) the daemon's `SetupConfig.models.*` filename stems
    //          — this is what the daemon actually loaded, and the
    //          daemon advertises those IDs on `/v1/models`,
    //       c) fallback: probe `/v1/models` and pick the first
    //          chat- and first embed-shaped entries.
    //    The historical (c)-only path picked non-deterministically
    //    between a locally-loaded `qwen-embedding-0.6b` (1024-dim)
    //    and a mesh-peer-advertised `Qwen3-Embedding-0.6B-Q8_0`
    //    whose dimensionality didn't match any installed corpus —
    //    silently downgrading every retrieval to FTS-only. Reading
    //    the config directly removes that race.
    let (chat_model, embed_model) = resolve_model_ids(&v1, globals).await?;
    eprintln!("Daemon: {base}");
    eprintln!("Chat model:  {chat_model}");
    eprintln!("Embed model: {embed_model}");

    // B:P9a — prefer the daemon's own OICP capabilities manifest for the chat
    // slot's context window (v0.4 §7) and the embed slot's query-instruction
    // prefix (§4), so `Runtime`'s budget-aware compaction sees the host's REAL
    // window (e.g. 32768) instead of the historical 8192 approximation. On a
    // v0.3 host that doesn't serve `/oicp/v1/capabilities`, fall back to 8192 +
    // the `DEFAULT_MANIFEST`-derived prefix (the prior behavior, bit-identical).
    let inference: Arc<dyn InferenceProvider> = Arc::new(
        match sovereign_inference::remote::fetch_manifest(&base, globals.bearer.clone()).await {
            Some(manifest) => SplitInferenceProvider::from_manifest_with_bearer(
                &v1,
                globals.bearer.clone(),
                &manifest,
                chat_model,
                embed_model.clone(),
            ),
            None => SplitInferenceProvider::new_with_bearer(
                &v1,
                globals.bearer.clone(),
                chat_model,
                embed_model.clone(),
                8192,
                sovereign_core::models_manifest::DEFAULT_MANIFEST
                    .embed_query_instruction(&embed_model),
            ),
        },
    );

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

    // 4. Build the CorpusEngine. The desktop (`state.rs`) hardcodes
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
    let (recipes_dir, indexes_dir): (PathBuf, PathBuf) = if globals.data_dir_explicit {
        (
            globals.data_dir.join("recipes"),
            globals.data_dir.join("indexes"),
        )
    } else {
        (dotsovereign.join("recipes"), dotsovereign.join("indexes"))
    };
    eprintln!("Indexes:     {}", indexes_dir.display());
    let embed_fn = sovereign_tools::corpus::inference_to_embed_fn(Arc::clone(&inference));
    let inference_fn = sovereign_tools::corpus::inference_to_inference_fn(Arc::clone(&inference));
    // The engine's `expected_embedding_model` flows into
    // `_corpus_meta.json` at ingest time and into shard-consistency
    // checks. The CLI doesn't ingest during chat, but if any tool
    // path later triggers an ingest (e.g. watcher-driven reindex
    // through the same engine), it must match what the desktop
    // would have written.
    let corpus_engine = Arc::new(
        corpus_engine::CorpusEngine::new(recipes_dir, indexes_dir.clone(), embed_fn)
            .with_embedding_model(&embed_model)
            .with_inference_fn(inference_fn),
    );

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

    // 6. Tool-Mastery Layer 3 — NoteStore for the per-conversation
    //    `tool_decision` write hook. Same path the daemon uses, so the chat
    //    REPL and bench surfaces share one outcome log.
    let notes_path = globals.data_dir.join("notes.db");
    let note_store = match corpus_engine_notes::NoteStore::open(&notes_path) {
        Ok(s) => Some(Arc::new(s)),
        Err(e) => {
            eprintln!(
                "warn: NoteStore open failed at {} ({e}); tool-decision \
                 writes will no-op this session",
                notes_path.display()
            );
            None
        }
    };

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
            corpus_engine: Arc::clone(&corpus_engine),
            // Cloned: `tool_bundles` below borrows the same handle for
            // `knowledge_lookup`'s notes channel. One store, two readers.
            note_store: note_store.clone(),
            skills: Arc::clone(&skills),
            // Chat turns don't trigger confirmations in the normal path; a
            // yes-only stub keeps a stray approval request from deadlocking a
            // one-shot CLI.
            approval: Arc::new(AutoApprove) as Arc<dyn ApprovalChannel>,
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
                        corpus_engine: &corpus_engine,
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
                b.push(Box::new(sovereign_tools::bundles::WikipediaTools::new(
                    Arc::clone(&corpus_engine),
                )));
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
            // The provider here is a `SplitInferenceProvider` over HTTP and
            // does not support rerank, so a standalone slot is the only way
            // this surface gets a cross-encoder at all.
            rerank: RerankWiring::Standalone,
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
        match sovereign_mesh::knowledge_client::MeshKnowledgeClient::new(&base) {
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
        corpus_engine,
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

impl RecipeProgress for Banner {
    fn note(&self, line: &str) {
        eprintln!("{line}");
    }
}

/// GET `/v1/models` with a 2s timeout. Any non-200 aborts bootstrap
/// with a clear remediation hint — the alternative is cryptic
/// "connection refused" errors minutes later, mid-retrieval.
/// `bearer` is `Some` only under a guest link. `/v1/models` is NOT in
/// `AUTH_EXEMPT_PATHS`, so probing it without the bearer against a lender's
/// node returns 401 and the message would blame the daemon rather than the
/// missing credential. (`/status` is exempt, but it does not prove the
/// inference surface is reachable, which is what this probe is for.)
async fn probe_or_bail(base: &str, bearer: Option<&str>) -> Result<()> {
    let url = format!("{base}/v1/models");
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(2))
        .build()
        .map_err(|e| Error::Serialization(format!("http client build: {e}")))?;
    let mut req = client.get(&url);
    if let Some(t) = bearer {
        req = req.bearer_auth(t);
    }
    match req.send().await {
        Ok(r) if r.status().is_success() => Ok(()),
        Ok(r) => Err(Error::Serialization(format!(
            "daemon at {base} returned {} from /v1/models. \
             Is it really a svrn daemon? Try `svrn doctor`.",
            r.status()
        ))),
        Err(_) => Err(Error::Serialization(format!(
            "daemon unreachable at {base}. \
             Start it with `svrn daemon run`, or pass --daemon <url>. \
             (If this is a guest link, the lending node is down or unreachable — \
             `svrn mesh use --forget` returns you to your own daemon.)"
        ))),
    }
}

/// Resolve `(chat_model_id, embed_model_id)` against the daemon.
/// See the call-site comment in `build_session` for the preference
/// order — explicit flag → SetupConfig stem → `/v1/models` probe.
async fn resolve_model_ids(v1: &str, globals: &ChatGlobals) -> Result<(String, String)> {
    // (a) Explicit flags short-circuit everything.
    if let (Some(c), Some(e)) = (&globals.chat_model, &globals.embed_model) {
        return Ok((c.clone(), e.clone()));
    }

    // Under a guest link the local `SetupConfig` names THIS machine's models,
    // and the guest wants the LENT one. Reading config here would name a model
    // the turn was not borrowed for.
    //
    // Falling through to `/v1/models` is now the honest source in a second
    // sense: since 2026-08-28 that listing is our OWN daemon's, and it carries
    // the granted ids alongside local slots (`lender_manifest`). So the id
    // this resolves to is one the local daemon can actually route — which is
    // the whole point, because the turn runs there and only the completion
    // crosses.
    let guest = globals.guest_link_active;

    // The EMBED id goes through the one decider
    // (`sovereign_workflow_host::daemon_models`, ARCH §10.6): explicit flag →
    // configured stem → embedding-like advertised id, then a
    // `/v1/embeddings` probe so a session never starts on an embed model the
    // daemon cannot answer with. This file used to hold its own copy of the
    // ladder (stem first, then an `embedding`/`-embed` substring), and
    // `recipe_cmd` + `workflow-host` each held a different one — which is how
    // one daemon could serve `svrn chat` and refuse `svrn corpus ingest`.
    //
    // Under a guest link the configured stem is THIS machine's and is
    // skipped; a grant with no embed model is tolerated with the sentinel
    // below rather than refused, so the guest branch keeps its own path
    // through the `/v1/models` loop.
    let mut embed_found = if guest {
        globals.embed_model.clone()
    } else {
        Some(
            sovereign_workflow_host::resolve_embed_model(v1, globals.embed_model.as_deref())
                .await
                .map_err(Error::Serialization)?
                .id,
        )
    };

    // (b) The CHAT stem from SetupConfig. The daemon loads
    //     `config.models.primary` and advertises it on `/v1/models`
    //     under its filename stem. Preferring the stem over
    //     `/v1/models` iteration means we always reach the
    //     *local* slot, never a mesh-peer advertisement, and the
    //     answer is stable across invocations.
    let mut chat_found =
        globals
            .chat_model
            .clone()
            .or_else(|| if guest { None } else { chat_stem_from_config() });
    if let (Some(c), Some(e)) = (chat_found.as_ref(), embed_found.as_ref()) {
        return Ok((c.clone(), e.clone()));
    }

    // (c) Fallback: probe `/v1/models`. Used when SetupConfig is
    //     absent (fresh install, dev without setup) or when it
    //     lacks one of the two slots.
    let url = format!("{v1}/models");
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .map_err(|e| Error::Serialization(format!("http client build: {e}")))?;
    let mut req = client.get(&url);
    if let Some(t) = globals.bearer.as_deref() {
        req = req.bearer_auth(t);
    }
    let resp = req
        .send()
        .await
        .map_err(|e| Error::Serialization(format!("GET {url}: {e}")))?;
    if !resp.status().is_success() {
        return Err(Error::Serialization(format!(
            "GET {url} returned {}",
            resp.status()
        )));
    }
    let v: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| Error::Serialization(format!("parse /v1/models: {e}")))?;
    let arr = v
        .get("data")
        .and_then(|d| d.as_array())
        .ok_or_else(|| Error::Serialization("/v1/models: no `data` array".into()))?;
    for m in arr {
        let Some(id) = m.get("id").and_then(|s| s.as_str()) else {
            continue;
        };
        let is_embed = sovereign_workflow_host::looks_like_embed_model(id);
        // Under a guest link the chat model must be one the LENDER
        // advertises. This listing carries local slots, mesh peers AND the
        // lender's granted ids, and taking whichever non-embed id comes
        // first means a guest borrows a model and then asks their own local
        // slot the question. That is exactly what the first live 3.3 did:
        // the turn served from `Qwen3.8-27B-UD-Q6_K_XL` on this machine
        // while a grant for the lender's model sat unused.
        //
        // `advertised_by` is the daemon's own answer to "who holds this",
        // so there is no second scope lookup here (§10.6).
        let lender_holds = |m: &serde_json::Value| -> bool {
            let Some(lender) = globals.guest_lender_url.as_deref() else {
                return false;
            };
            m.get("advertised_by")
                .and_then(|a| a.as_array())
                .is_some_and(|rows| rows.iter().any(|h| h.as_str() == Some(lender)))
        };
        if is_embed {
            if embed_found.is_none() {
                embed_found = Some(id.to_string());
            }
        } else if chat_found.is_none() && (!guest || lender_holds(m)) {
            chat_found = Some(id.to_string());
        }
    }

    match (chat_found, embed_found) {
        (Some(c), Some(e)) => Ok((c, e)),
        // Under a guest link this branch has ONE cause and it is not the
        // local slots: the lender advertises nothing under this grant, so it
        // expired, was revoked, or the lending node restarted (grants are
        // held in memory). Sending the operator to `svrn setup` for that is
        // the B1 misattribution shape — a true statement about the wrong
        // subject. This is the surface live bar 3.6 reads.
        (None, _) if guest => Err(Error::Serialization(
            "the guest link is live but the lending node grants no chat model — the grant has \
             expired, been revoked, or the lender restarted (grants are held in memory). \
             Nothing was served from this node in its place; `svrn mesh use --forget` drops \
             the link."
                .into(),
        )),
        (None, _) => Err(Error::Serialization(
            "daemon lists no chat models — check `svrn setup` and the primary/fast slots".into(),
        )),
        // A guest grant may cover chat and no embedding model at all — today
        // `Scope::Models` unlocks `/v1/chat/completions`, not `/v1/embeddings`.
        // That is not a reason to refuse the whole session: chat works. It IS
        // a reason never to substitute a plausible-looking id, which would make
        // retrieval appear to work and quietly return nothing (§18.3). The
        // sentinel is unservable BY DESIGN — an embed call fails with the host
        // naming this exact string.
        (Some(c), None) if guest => {
            eprintln!(
                "Guest link: this grant covers chat only — no embedding model is in scope, \
                 so retrieval over local corpora will be refused by the lending node."
            );
            Ok((c, NO_EMBED_MODEL_IN_GRANT.to_string()))
        }
        (_, None) => Err(Error::Serialization(
            "daemon lists no embedding model — retrieval will fail. Set `[models] embed` in \
             ~/.svrnmesh/config.toml or pass --embed-model."
                .into(),
        )),
    }
}

/// Stand-in embed-model id when a guest grant names no embedding model.
///
/// Deliberately not a real name, and deliberately not the chat model's: any
/// embed request fails with the host quoting THIS string back, which says
/// exactly what happened. A plausible substitute would make the failure look
/// like a retrieval miss.
const NO_EMBED_MODEL_IN_GRANT: &str = "(no-embedding-model-in-this-guest-grant)";

/// Filename stem of `SetupConfig.models.primary`. The daemon advertises it
/// on `/v1/models` using exactly the file stem, so this is the stable
/// local chat id without a `/v1/models` round-trip. (The embed stem's
/// twin lives in `sovereign_workflow_host::daemon_models`.)
fn chat_stem_from_config() -> Option<String> {
    sovereign_core::setup_config::SetupConfig::load()
        .ok()?
        .primary_model_stem()
}

/// Approval channel that silently yes-answers everything. Chat never
/// hits the ask-user path in practice; this prevents a surprise
/// deadlock in a one-shot CLI invocation.
struct AutoApprove;

#[async_trait]
impl ApprovalChannel for AutoApprove {
    async fn request_approval(&self, _step: &Step, _preview: &ActionPreview) -> Result<bool> {
        Ok(true)
    }

    async fn ask_user(&self, _question: &str) -> Result<String> {
        Ok(String::new())
    }

    fn emit_progress(&self, _step: &Step, _output: &StepOutput) {}
}
