// SPDX-License-Identifier: AGPL-3.0-or-later
//! The attached-document probe: svrn builds (or reuses) a document asset,
//! meters the build, and answers each question through a minted
//! `DocumentSession` turn (pb-bench-dials-docs).

use std::panic::AssertUnwindSafe;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use futures::FutureExt;
use sovereign_contracts::probe::{
    AttachedEvidence, AttachedProbe, AttachedSource, AttachedTurn, ProbeQuestion, StateTransition,
};
use sovereign_core::runtime::Runtime;
use sovereign_core::traits::{InferenceProvider, StateStore};
use sovereign_core::types::{DocumentAsset, DocumentSession, Message, NarrationEvent, Role, Speed};
use sovereign_tools::document_asset::{DocumentAssetManager, IngestProgress};

use super::resource_meter::{MeteredInference, ResourceLedger};
use crate::chat_cmd::bootstrap::{ChatSession, SplitInferenceProvider};

/// Run the attached probe: list the store's assets, or resolve the asset
/// (ingest or reuse, with the build knobs), meter the build, and answer
/// every question through [`dispatch_question`].
pub(super) async fn probe(
    session: &ChatSession,
    daemon_base: &str,
    corpus: &str,
    questions: &[ProbeQuestion],
    spec: &AttachedProbe,
) -> Result<AttachedEvidence, String> {
    let chat_model = session.inference.model_id_for(Speed::Slow);
    let ledger = Arc::new(ResourceLedger::new());
    let mut evidence = AttachedEvidence {
        assets: Vec::new(),
        asset: None,
        chat_model: chat_model.clone(),
        enrich_model: chat_model,
        attach_ms: 0,
        transitions: Vec::new(),
        terminal_phase: String::new(),
        chunks: Vec::new(),
        rows: Vec::new(),
        resources: ledger.snapshot(),
    };
    if let AttachedSource::List = spec.source {
        evidence.assets = session
            .store
            .list_document_assets()
            .await
            .map_err(|e| format!("list_document_assets: {e}"))?;
        tracing::debug!(assets = evidence.assets.len(), "attached probe: listed");
        return Ok(evidence);
    }
    if spec.warm_atlas {
        warm_atlas(session, corpus, &spec.lane).await;
    }

    // Enrichment provider: same as the session's unless `enrich_model`
    // splits it, then wrapped in the metering decorator so every
    // skeleton/RAPTOR/embed call lands in the per-phase resource ledger.
    let enrich_base: Arc<dyn InferenceProvider> = match &spec.enrich_model {
        Some(model) => {
            eprintln!("      enrich model override: {model}");
            provider_for_model(daemon_base, model, &session.embed_model).await
        }
        None => Arc::clone(&session.inference),
    };
    let enrich_inference: Arc<dyn InferenceProvider> =
        Arc::new(MeteredInference::new(enrich_base, Arc::clone(&ledger)));
    evidence.enrich_model = enrich_inference.model_id_for(Speed::Slow);
    let mut manager =
        DocumentAssetManager::new(Arc::clone(&enrich_inference), Arc::clone(&session.store));
    // The entity pass runs only when the manager builds something.
    let builds = matches!(spec.source, AttachedSource::Ingest { .. })
        || spec.rebuild_skeleton
        || spec.rebuild_raptor;
    if builds {
        if let Some(g) = entity_extractor(spec).await {
            manager = manager.with_entity_extractor(g);
        }
    }

    let (asset, attach_ms, transitions, terminal_phase) = match &spec.source {
        AttachedSource::Ingest { path } => attach_and_stream(&manager, path, &ledger).await,
        AttachedSource::Reuse { asset_id } => {
            reuse(session.store.as_ref(), &manager, &ledger, asset_id, spec).await?
        }
        AttachedSource::List => unreachable!("answered above"),
    };
    // Any enrich-provider calls after ingest (none expected) get their
    // own bucket rather than polluting the last pipeline phase.
    ledger.set_phase("post_ingest");
    tracing::debug!(
        asset = asset.as_ref().map(|a| a.id.as_str()),
        terminal_phase = %terminal_phase,
        attach_ms,
        "attached probe: asset resolved"
    );

    if let Some(a) = &asset {
        evidence.chunks = match session.store.get_chunks_by_source(&a.source_key()).await {
            Ok(c) => c,
            Err(e) => {
                eprintln!(
                    "      [attached] read chunks for {}: {e}; judging evidence is empty",
                    a.id
                );
                Vec::new()
            }
        };
        for q in questions {
            evidence.rows.push(turn(session, a, q).await);
        }
    }
    evidence.asset = asset;
    evidence.attach_ms = attach_ms;
    evidence.transitions = transitions;
    evidence.terminal_phase = terminal_phase;
    evidence.resources = ledger.snapshot();
    Ok(evidence)
}

/// One question: its embedding (the key a judge ranks the asset's chunks
/// by), then the turn, a panic caught into the row's error.
async fn turn(session: &ChatSession, asset: &DocumentAsset, q: &ProbeQuestion) -> AttachedTurn {
    eprintln!("      [{}] attached turn", q.id);
    let question_embedding = match session.inference.embed(&q.question).await {
        Ok(v) => v,
        Err(e) => {
            tracing::debug!(id = %q.id, error = %e, "attached probe: question embed failed");
            Vec::new()
        }
    };
    let started = Instant::now();
    // catch_unwind so a panic in the runtime doesn't lose every
    // prior question's data. AssertUnwindSafe is justified because
    // the future only holds Arc<dyn> refs + borrowed args — no
    // shared interior mutability that could observe a
    // half-poisoned state.
    let drive = AssertUnwindSafe(dispatch_question(
        &session.runtime,
        &session.store,
        asset,
        &q.question,
    ));
    let result = match drive.catch_unwind().await {
        Ok(r) => r,
        Err(payload) => Err(format!("panic: {}", panic_payload_to_string(&payload))),
    };
    let latency_ms = started.elapsed().as_millis() as u64;
    match result {
        Ok(ans) => AttachedTurn {
            id: q.id.clone(),
            error: None,
            answer: ans.text,
            metadata: ans.metadata,
            narration: ans.narration_log,
            latency_ms,
            question_embedding,
        },
        Err(e) => {
            eprintln!("        → turn failed: {e}");
            AttachedTurn {
                id: q.id.clone(),
                error: Some(e),
                answer: String::new(),
                metadata: None,
                narration: Vec::new(),
                latency_ms,
                question_embedding,
            }
        }
    }
}

/// T2 entity pass: prefer serve's NER model over the LLM when serve
/// holds one. Asked up front because the bench must measure the NER
/// path, not race it — an extractor that isn't there yet would make a
/// GLiNER run look like a no-op. `no_gliner` forces the LLM path for A/B
/// comparison.
async fn entity_extractor(
    spec: &AttachedProbe,
) -> Option<Arc<dyn sovereign_core::traits::EntityExtractor>> {
    if spec.no_gliner {
        eprintln!("      T2 entity pass: LLM (--no-gliner)");
        return None;
    }
    match crate::serve_dial::serve_ner(&spec.lane).await {
        Ok(Some(g)) => {
            eprintln!("      T2 entity pass: GLiNER ({}, serve's)", g.model_id());
            Some(Arc::new(sovereign_contracts::ner::NerEntities(g))
                as Arc<dyn sovereign_core::traits::EntityExtractor>)
        }
        Ok(None) => {
            eprintln!("      T2 entity pass: LLM (serve has no NER model)");
            None
        }
        Err(e) => {
            eprintln!("      T2 entity pass: LLM ({e})");
            None
        }
    }
}

/// Look up an existing asset and, when asked, rebuild its skeleton or its
/// RAPTOR atlas before any question. A rebuild that fails keeps the stored
/// data and says so.
async fn reuse(
    store: &dyn StateStore,
    manager: &DocumentAssetManager,
    ledger: &Arc<ResourceLedger>,
    reuse_id: &str,
    spec: &AttachedProbe,
) -> Result<(Option<DocumentAsset>, u64, Vec<StateTransition>, String), String> {
    eprintln!("[3/3] reuse — looking up existing asset {reuse_id}");
    let found = match store.get_document_asset(reuse_id).await {
        Ok(Some(found)) => found,
        Ok(None) => {
            return Err(format!(
                "no asset with id {reuse_id} in the daemon's store. Try --list-assets."
            ))
        }
        Err(e) => return Err(format!("lookup asset {reuse_id}: {e}")),
    };
    eprintln!(
        "      found: title=\"{}\" state={:?}",
        found.title, found.state
    );
    let asset_to_use = if spec.rebuild_skeleton {
        eprintln!("      --rebuild-skeleton: re-running skeleton extraction (uses current build_skeleton speed)");
        ledger.set_phase("rebuild_skeleton");
        let rebuild_start = std::time::Instant::now();
        match manager.rebuild_skeleton(reuse_id).await {
            Ok(new_skeleton) => {
                let secs = rebuild_start.elapsed().as_secs();
                eprintln!(
                    "      rebuild ok in {secs}s: {} entities, {} moments, {} actions",
                    new_skeleton.main_entities.len(),
                    new_skeleton.structural_moments.len(),
                    new_skeleton.actions.len(),
                );
                // Reload the asset to pick up the new skeleton.
                match store.get_document_asset(reuse_id).await {
                    Ok(Some(refreshed)) => refreshed,
                    _ => found,
                }
            }
            Err(e) => {
                eprintln!("      rebuild_skeleton failed: {e}; using existing skeleton");
                found
            }
        }
    } else {
        found
    };
    if spec.rebuild_raptor {
        eprintln!(
            "      --rebuild-raptor: populating RAPTOR atlas + motif index on the existing asset"
        );
        ledger.set_phase("rebuild_raptor");
        let raptor_start = std::time::Instant::now();
        match manager.rebuild_raptor_atlas(reuse_id).await {
            Ok(()) => {
                let secs = raptor_start.elapsed().as_secs();
                let node_count = store
                    .list_raptor_nodes(reuse_id)
                    .await
                    .map(|v| v.len())
                    .unwrap_or(0);
                let motif_count = store
                    .list_asset_motifs(reuse_id)
                    .await
                    .map(|v| v.iter().filter(|m| m.is_distinctive).count())
                    .unwrap_or(0);
                eprintln!(
                    "      raptor rebuild ok in {secs}s: {node_count} nodes, {motif_count} distinctive motifs"
                );
            }
            Err(e) => {
                eprintln!("      rebuild_raptor_atlas failed: {e}; continuing without RAPTOR data");
            }
        }
    }
    Ok((
        Some(asset_to_use),
        0u64,
        vec![StateTransition {
            ms_since_attach: 0,
            phase: "reused".to_string(),
            detail: serde_json::json!({ "asset_id": reuse_id }),
        }],
        "reused".to_string(),
    ))
}

/// Atlas grounding (opt-in): the lane seals to ONE corpus, so warm THAT
/// corpus's enrichment atlas into the session's manager (the same Arc the
/// Runtime queries) before any turn. Without it the manager is cache-only
/// and a freshly-enriched corpus with no embed cache contributes 0 atlas
/// contexts — the run would silently measure base chunk retrieval. The
/// filter (min-description-chars, include-claims) is env-configured; it is
/// echoed so the measurement stays glassbox.
async fn warm_atlas(session: &ChatSession, corpus: &str, lane: &str) {
    let f = corpus_engine::enrichment::atlas::context_loader::AtlasContextFilter::default();
    let n = session.atlas_mgr.warm_one(corpus).await;
    eprintln!(
        "[{lane}] atlas-warm: {n} context entr{} loaded for `{corpus}` (min_description_chars={}, include_claims={})",
        if n == 1 { "y" } else { "ies" },
        f.min_description_chars,
        f.include_claims,
    );
    if n == 0 {
        eprintln!(
            "[{lane}] WARN: atlas warm loaded 0 entries — this run measures BASE retrieval, NOT the atlas. \
             Relax the filter (SOVEREIGN_ATLAS_MIN_DESCRIPTION_CHARS=0 SOVEREIGN_ATLAS_INCLUDE_CLAIMS=1) and confirm an atlas exists for `{corpus}`."
        );
    }
}

pub(crate) use oicp_client::provider_for_model;

/// Map an `IngestProgress` event onto the bench's phase taxonomy. We
/// keep our own labels rather than re-exporting the tool's enum so the
/// JSON schema is stable across `sovereign-tools` refactors. The
/// `rag_available` label is what the bench measures as
/// `time_to_rag_ready_ms` — Tier 1+2 questions are answerable from
/// here on, even though the skeleton may still be building.
fn render_progress(p: &IngestProgress) -> (&'static str, serde_json::Value) {
    match p {
        IngestProgress::Started {
            word_count,
            chunk_count,
            filename,
            ..
        } => (
            "started",
            serde_json::json!({
                "word_count": word_count,
                "chunk_count": chunk_count,
                "filename": filename,
            }),
        ),
        IngestProgress::Indexing { done, total } => (
            "indexing",
            serde_json::json!({ "done": done, "total": total }),
        ),
        IngestProgress::RagAvailable { asset_id } => {
            ("rag_available", serde_json::json!({ "asset_id": asset_id }))
        }
        IngestProgress::BuildingSkeleton { done, total } => (
            "building_skeleton",
            serde_json::json!({ "done": done, "total": total }),
        ),
        IngestProgress::MultiHopReady { asset_id } => (
            "multi_hop_ready",
            serde_json::json!({ "asset_id": asset_id }),
        ),
        IngestProgress::Ready {
            asset_id,
            main_entities,
            structural_moments,
        } => (
            "ready",
            serde_json::json!({
                "asset_id": asset_id,
                "main_entities": main_entities,
                "structural_moments": structural_moments,
            }),
        ),
        IngestProgress::Failed { reason } => ("failed", serde_json::json!({ "reason": reason })),
    }
}

/// Drive `DocumentAssetManager::ingest()` end-to-end while recording
/// every `IngestProgress` transition with elapsed-ms timestamps.
/// Returns the completed asset (or `None` on ingest failure), the
/// total attach duration, the transition log, and the terminal phase
/// label.
async fn attach_and_stream(
    manager: &DocumentAssetManager,
    path: &Path,
    ledger: &Arc<ResourceLedger>,
) -> (Option<DocumentAsset>, u64, Vec<StateTransition>, String) {
    let attach_at = Instant::now();
    let transitions: Arc<Mutex<Vec<StateTransition>>> = Arc::new(Mutex::new(Vec::new()));
    let callback_transitions = Arc::clone(&transitions);
    let callback_attach_at = attach_at;
    let callback_ledger = Arc::clone(ledger);

    eprintln!("[3/3] attach — DocumentAssetManager::ingest()");
    eprintln!("      transitions stream below; rag_available unblocks Tier-1+2 questions");
    ledger.set_phase("attach_ingest");

    let ingest_result = manager
        .ingest(path, move |progress| {
            let elapsed = callback_attach_at.elapsed().as_millis() as u64;
            let (phase, detail) = render_progress(&progress);
            // The pipeline is sequential; the live progress phase is
            // the correct attribution bucket for resource accounting.
            callback_ledger.set_phase(phase);
            eprintln!("      t+{:>6}ms  {}", elapsed, phase);
            if let Ok(mut log) = callback_transitions.lock() {
                log.push(StateTransition {
                    ms_since_attach: elapsed,
                    phase: phase.to_string(),
                    detail,
                });
            }
        })
        .await;

    let attach_ms = attach_at.elapsed().as_millis() as u64;
    let (asset, terminal_phase) = match ingest_result {
        Ok(a) => (Some(a), "ready".to_string()),
        Err(e) => {
            eprintln!("      ingest failed after t+{attach_ms}ms: {e}");
            if let Ok(mut log) = transitions.lock() {
                log.push(StateTransition {
                    ms_since_attach: attach_ms,
                    phase: "failed".to_string(),
                    detail: serde_json::json!({ "error": e.to_string() }),
                });
            }
            (None, "failed".to_string())
        }
    };
    let transitions_vec = transitions.lock().map(|g| g.clone()).unwrap_or_default();
    (asset, attach_ms, transitions_vec, terminal_phase)
}

/// Best-effort string view of a panic payload — `panic!("foo")` boxes
/// a `&'static str`; `panic!("{}", x)` boxes a `String`; anything else
/// falls back to `<non-string panic>`. Used to surface upstream panics
/// in the bench's per-question error column without losing the rest of
/// the run.
fn panic_payload_to_string(payload: &Box<dyn std::any::Any + Send>) -> String {
    if let Some(s) = payload.downcast_ref::<&'static str>() {
        return (*s).to_string();
    }
    if let Some(s) = payload.downcast_ref::<String>() {
        return s.clone();
    }
    "<non-string panic>".to_string()
}

/// One successful question dispatch: the assistant message's content and
/// metadata, and `narration_log`, the per-phase events the runtime
/// emitted — the load-bearing diagnostic for whether the model chose to
/// consult the attached document.
struct DispatchedAnswer {
    text: String,
    metadata: Option<serde_json::Value>,
    narration_log: Vec<NarrationEvent>,
}

/// Dispatch a single question through the runtime's normal turn
/// pipeline. Fresh `conversation_id` per question so context doesn't
/// leak between bench items; a fresh `DocumentSession` pinned to that
/// conversation tells the runtime an attachment is in scope so it
/// dispatches through `handle_attached_doc_turn` instead of the
/// general-purpose intent handlers.
///
/// **What this used to be.** Before 2026-05-20 this function mirrored
/// the desktop's pre-tool-era `ask_document` flow: call
/// `DocumentAssetManager::route()` first, fall back to
/// `runtime.handle_turn` only on `OffTopic`, otherwise dispatch
/// through `manager.ask()`. The book-report bench exposed that the
/// parallel router mis-routed factual questions about the attached
/// novel as `OffTopic` — sending them to the general corpus when
/// the answer was in the attached text — and that even when it did
/// route correctly, `manager.ask` ran a parallel one-shot map-reduce
/// with no gap-check, no iterative retrieval, and no narration. See
/// sovereign decision `7693f16b`.
///
/// **What it is now.** A thin shim that creates a `DocumentSession`
/// pointing at the Ready asset, then drives the runtime's turn
/// pipeline. `Runtime::handle_turn` detects the session and routes
/// through `handle_attached_doc_turn` — a `ReasonWithTools`-style loop
/// over `[attached_doc_search, knowledge_lookup, web_fetch]` where the
/// model picks tools.
async fn dispatch_question(
    runtime: &Arc<Runtime>,
    store: &Arc<dyn StateStore>,
    asset: &DocumentAsset,
    prompt: &str,
) -> Result<DispatchedAnswer, String> {
    let conversation_id = uuid::Uuid::new_v4().to_string();
    let user_msg = Message {
        id: uuid::Uuid::new_v4().to_string(),
        conversation_id: conversation_id.clone(),
        role: Role::User,
        content: prompt.to_string(),
        created_at: chrono::Utc::now().timestamp(),
        metadata: None,
        version: 0,
    };
    store
        .save_message(&user_msg)
        .await
        .map_err(|e| format!("save user msg: {e}"))?;

    // Mint a DocumentSession so the runtime detects the attachment
    // and routes through `handle_attached_doc_turn`. The session is
    // intentionally minimal — `operation` / `map_prompt` /
    // `reduce_prompt` are the legacy map-reduce path's fields and
    // aren't consulted by the new tool-loop handler. We leave them as
    // empty strings rather than inventing values.
    let session = DocumentSession {
        id: uuid::Uuid::new_v4().to_string(),
        conversation_id: conversation_id.clone(),
        filename: asset.title.clone(),
        source: asset.id.clone(),
        word_count: 0,
        chunk_count: 0,
        created_at: chrono::Utc::now().timestamp(),
        operation: String::new(),
        map_prompt: String::new(),
        reduce_prompt: String::new(),
        last_output: None,
        history: Vec::new(),
    };
    store
        .create_document_session(&session)
        .await
        .map_err(|e| format!("create document session: {e}"))?;

    let response = runtime
        .handle_turn(prompt, &conversation_id)
        .await
        .map_err(|e| format!("runtime: {e}"))?;

    // Capture the runtime's narration for this question. The
    // SessionStore retains the latest QuerySession per conversation
    // for 30s after completion, so this read is race-free as long
    // as the bench doesn't churn through questions faster than that.
    let narration_log = runtime
        .sessions
        .latest_for_conversation(&conversation_id)
        .map(|s| s.narration.clone())
        .unwrap_or_default();

    Ok(DispatchedAnswer {
        text: response.message.content,
        metadata: response.message.metadata,
        narration_log,
    })
}
