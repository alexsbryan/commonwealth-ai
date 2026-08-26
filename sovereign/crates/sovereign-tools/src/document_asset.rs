// SPDX-License-Identifier: AGPL-3.0-or-later
//! Document Asset Manager — ingest once, query forever.
//!
//! Handles the full lifecycle of a document asset:
//!
//! 1. **Ingest** — parse, chunk, embed, and build a structural skeleton.
//!    Embedding and skeleton extraction run concurrently. Once embedding
//!    completes, RAG queries are available while the skeleton keeps building.
//!
//! 2. **Route** — classify a user's question into one of four operation
//!    types (RAG, Synthesis, Aggregation, Transformation) using the
//!    skeleton's overview and the question text.
//!
//! 3. **Execute** — run the selected operation and return the response
//!    with source citations.
//!
//! Reuses the existing RAG pipeline (`rag::parse`, `rag::chunk`) for
//! parsing and chunking. Reuses `DocumentOperationTool`'s map-reduce
//! pattern for the synthesis and aggregation executors.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use futures::stream::{self, StreamExt};
use serde::Deserialize;
use sovereign_core::error::{Error, Result};
use sovereign_core::slot_policy::Workload;
use sovereign_core::traits::{EntityExtractor, InferenceProvider, StateStore};
use sovereign_core::types::*;

use crate::rag::chunk::{chunk_text, TextChunk};
use crate::rag::parse::parse_file;
use crate::raptor_atlas::ChunkInput;

/// Concurrency cap for parallel per-batch LLM calls in the T2 entity
/// extraction phase. The mesh load balancer sees this many simultaneous
/// requests and fans them across peers; on single-machine deployments
/// the skeleton batches are Fast-class (EnrichBulk since 2026-07-24) so
/// the fan-out lands on the FastShort continuous-batching companion —
/// 12 keeps its per-sequence KV slices fed without queueing beyond the
/// slot's n_seq. (Historical: 6, tuned for a 2-peer mesh when the
/// batches were Normal-class and a single local slot serialized them.)
const T2_BATCH_CONCURRENCY: usize = 12;

// ─── Self-reference detection ────────────────────────────────

/// Lowercase substrings that unambiguously mark a question as directed at
/// the attached document. Matching any of these short-circuits the
/// LLM-based router — we don't need a Fast-slot call to know that
/// "summarize this document" is about the document.
///
/// Kept as a flat list rather than regex for predictability and cost:
/// a substring scan over ~100 chars is microseconds; a regex is overkill.
const SELF_REFERENCE_PHRASES: &[&str] = &[
    // "this <thing>" phrasings
    "this document",
    "this doc",
    "this pdf",
    "this file",
    "this text",
    "this paper",
    "this article",
    "this book",
    "this chapter",
    "this essay",
    "this report",
    // "the <thing>" phrasings — risk of false positive is low because
    // general-knowledge questions rarely mention "the document" etc.
    "the document",
    "the text",
    "the paper",
    "the article",
    "the book",
    "the chapter",
    "the essay",
    "the report",
    "the attached",
    // imperative summary/analysis phrasings
    "summarize this",
    "summarise this",
    "summary of this",
    "summarize the",
    "summarise the",
    // open-ended "what does/is this" patterns
    "what is this about",
    "what's this about",
    "what is this document",
    "what does this",
];

/// Return true when `request` explicitly references the attached document.
/// Case-insensitive substring match against [`SELF_REFERENCE_PHRASES`].
fn detect_self_reference(request: &str) -> bool {
    let q = request.to_lowercase();
    SELF_REFERENCE_PHRASES.iter().any(|p| q.contains(p))
}

/// Common English function words + digit-only tokens are dropped when
/// extracting filename keywords — a question that mentions "the" or "2024"
/// is not meaningfully "about" the attached document.
const FILENAME_STOPWORDS: &[&str] = &[
    "the", "and", "for", "but", "not", "you", "are", "with", "this", "that", "from", "into",
    "onto", "upon", "have", "had", "has", "was", "were", "been", "being", "its", "their", "them",
    "they", "our", "his", "her", "what", "which", "who", "whom", "when", "where", "why", "how",
    "too", "also", "just", "only", "pdf", "doc", "docx", "txt", "pages", "page", "chapter", "part",
    "vol", "volume", "edition", "copy", "draft", "final", "version", "revised",
];

/// ASCII-fold a string: strip diacritics so `"Schrödinger"` and
/// `"schrodinger"` compare equal. Lightweight char-by-char mapping that
/// covers the common Latin-1 Supplement range; sufficient for English
/// filenames with occasional accented loanwords.
fn ascii_fold(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            'à' | 'á' | 'â' | 'ã' | 'ä' | 'å' => 'a',
            'À' | 'Á' | 'Â' | 'Ã' | 'Ä' | 'Å' => 'A',
            'ç' => 'c',
            'Ç' => 'C',
            'è' | 'é' | 'ê' | 'ë' => 'e',
            'È' | 'É' | 'Ê' | 'Ë' => 'E',
            'ì' | 'í' | 'î' | 'ï' => 'i',
            'Ì' | 'Í' | 'Î' | 'Ï' => 'I',
            'ñ' => 'n',
            'Ñ' => 'N',
            'ò' | 'ó' | 'ô' | 'õ' | 'ö' | 'ø' => 'o',
            'Ò' | 'Ó' | 'Ô' | 'Õ' | 'Ö' | 'Ø' => 'O',
            'ù' | 'ú' | 'û' | 'ü' => 'u',
            'Ù' | 'Ú' | 'Û' | 'Ü' => 'U',
            'ý' | 'ÿ' => 'y',
            'Ý' => 'Y',
            'ß' => 's',
            other => other,
        })
        .collect()
}

/// Extract content-word tokens from a document's title + filename.
///
/// Splits on any non-alphabetic character (so `11._Erwin_Schrodinger_-_What_is_Life__1944_.pdf`
/// yields `["erwin", "schrodinger", "what", "is", "life"]`), lowercases,
/// ASCII-folds diacritics, drops tokens shorter than 3 chars, drops
/// stopwords, de-duplicates.
fn filename_tokens(asset: &DocumentAsset) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    let sources = [asset.title.as_str(), asset.filename.as_str()];
    for s in sources {
        let folded = ascii_fold(&s.to_lowercase());
        for tok in folded.split(|c: char| !c.is_ascii_alphabetic()) {
            if tok.len() < 3 {
                continue;
            }
            if FILENAME_STOPWORDS.contains(&tok) {
                continue;
            }
            if seen.insert(tok.to_string()) {
                out.push(tok.to_string());
            }
        }
    }
    out
}

/// Represents one chunk that contributed to an answer, with enough
/// metadata for the frontend to render a rich citation popover.
///
/// The `label` is the string the model uses when citing this chunk —
/// `"§4"` for a synthesis section, `"passage 2"` for a RAG match, etc.
/// The frontend matches it against `[Source: <label>]` spans in the
/// prose and, on click, shows the `snippet` in a popover keyed by
/// `corpus_id` (which the Tauri handler fills with the document title).
#[derive(Debug, Clone, serde::Serialize)]
pub struct CitedChunk {
    pub label: String,
    pub chunk_index: usize,
    pub content: String,
    pub snippet: String,
}

/// The full execution result handed back to `ask_document`. Bundles
/// everything needed to persist a rich assistant message — response
/// text, citation metadata, and the inference backend's own provenance
/// (model id + token count) which would otherwise be dropped on the
/// floor by `execute_*`.
#[derive(Debug, Clone)]
pub struct ExecutionOutput {
    pub text: String,
    pub citations: Vec<CitedChunk>,
    pub model_id: String,
    pub tokens_used: usize,
    pub latency_ms: u64,
    /// Why the synthesizing model stopped emitting. Plumbed from
    /// the underlying `CompletionResponse.finish_reason` so the
    /// desktop's DocumentAsk surface can light up the cutoff chip
    /// when ask_document's reply was length-truncated.
    pub finish_reason: Option<sovereign_core::types::FinishReason>,
    /// Completion-only token count from the synthesizing model.
    /// Mirrors `CompletionResponse.completion_tokens`.
    pub completion_tokens: Option<u32>,
}

impl ExecutionOutput {
    /// Sentinel used when a path decides there's nothing to do — e.g.
    /// `execute_rag` finds zero relevant chunks and signals the caller
    /// to fall through to the runtime pipeline.
    fn empty() -> Self {
        Self {
            text: String::new(),
            citations: Vec::new(),
            model_id: String::new(),
            tokens_used: 0,
            latency_ms: 0,
            finish_reason: None,
            completion_tokens: None,
        }
    }
}

/// First `max` *bytes* of `content` — walked back to the nearest UTF-8
/// char boundary, then to a whitespace boundary when one is available
/// within the safe window. Matches the snippet format used elsewhere in
/// the codebase so citation popovers look consistent with
/// knowledge-query popovers.
///
/// The char-boundary walk is load-bearing for non-ASCII documents:
/// Conrad uses curly quotes (U+201C, 3 bytes), em-dashes (U+2014, 3
/// bytes), and ellipses (U+2026, 3 bytes); slicing at a raw byte index
/// in that text used to panic with "end byte index N is not a char
/// boundary" inside the Cargo runtime.
fn short_snippet(content: &str, max: usize) -> String {
    if content.len() <= max {
        return content.to_string();
    }
    // Walk back to the nearest char boundary. is_char_boundary is
    // stable since 1.9; floor_char_boundary is nightly-only, so we
    // open-code the walk. At most 3 iterations (UTF-8 chars are ≤4
    // bytes), so the cost is negligible.
    let mut end = max;
    while end > 0 && !content.is_char_boundary(end) {
        end -= 1;
    }
    let truncated = &content[..end];
    match truncated.rfind(char::is_whitespace) {
        Some(pos) if pos > 0 => format!("{}...", &truncated[..pos]),
        _ => format!("{truncated}..."),
    }
}

/// True when `request` mentions any of `tokens` as a whole word. The
/// question is ASCII-folded + lowercased before comparison so e.g. the
/// token `"schrodinger"` matches `"Schrödinger"` in the user's question.
fn mentions_document(tokens: &[String], request: &str) -> bool {
    if tokens.is_empty() {
        return false;
    }
    let q = ascii_fold(&request.to_lowercase());
    // Split query into alphabetic words; a token matches if it appears
    // as one of those words. Avoids `"life"` false-matching in `"wildlife"`.
    let words: std::collections::HashSet<&str> = q
        .split(|c: char| !c.is_ascii_alphabetic())
        .filter(|w| !w.is_empty())
        .collect();
    tokens.iter().any(|t| words.contains(t.as_str()))
}

// ─── Progress types ──────────────────────────────────────────

/// Progress updates emitted during document ingest. The frontend
/// listens to these via Tauri events to drive the progress bar
/// and ingest banner.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(tag = "type")]
pub enum IngestProgress {
    /// File parsed, chunking complete, embedding about to start.
    /// Carries `asset_id` so the UI can route this earliest signal to
    /// the right banner — it's the same id every later event uses.
    Started {
        word_count: usize,
        chunk_count: usize,
        filename: String,
        asset_id: String,
    },
    /// Embedding chunks into the vector store.
    Indexing { done: usize, total: usize },
    /// Embedding complete. RAG queries now available. (T1 done)
    RagAvailable { asset_id: String },
    /// Skeleton enrichment in progress. The same variant fires for
    /// both the T2 (entity extraction) and T3 (RAPTOR + segments +
    /// overview) phases; the `MultiHopReady` event between them
    /// signals the phase boundary.
    BuildingSkeleton { done: usize, total: usize },
    /// T2 done. Entity index + action atoms available; PPR
    /// multi-hop retrieval works while T3 continues.
    MultiHopReady { asset_id: String },
    /// T3 done. All operations available.
    Ready {
        asset_id: String,
        main_entities: usize,
        structural_moments: usize,
    },
    /// Ingest failed.
    Failed { reason: String },
}

/// Progress updates during a query operation. The frontend shows
/// these as loading state text below the input.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(tag = "type")]
pub enum OperationProgress {
    /// Router has decided which operation to use.
    Routing { operation: String },
    /// Retrieving relevant passages from the index.
    Retrieving,
    /// Analysing a specific entity across the document.
    AnalysingEntity { name: String },
    /// Synthesising the final response.
    Synthesising,
}

/// Output of the inference-free preparation step: the persisted
/// `Pending` asset plus the chunked text awaiting embedding.
///
/// The asset id minted in `prepare` is the SAME id `run_ingest` emits
/// every `document:progress` event under. The desktop command returns
/// this id to the UI *before* spawning `run_ingest`, so the banner the
/// UI shows and the events it receives agree on one id. Before this
/// split the command minted one id and `ingest` minted another
/// internally — the banner subscribed to an id that never received a
/// single progress event and sat on "Queued…" for the entire ingest.
pub struct PreparedIngest {
    pub asset: DocumentAsset,
    text_chunks: Vec<TextChunk>,
}

// ─── Manager ─────────────────────────────────────────────────

/// Manages the lifecycle of document assets: ingest, route, execute.
///
/// Holds references to inference (for embedding + LLM calls) and
/// storage (for persisting assets and chunks). Does not own a
/// CorpusEngine — document assets use the existing `DocumentStore`
/// chunk storage with FTS5 search, not LanceDB corpus indexes.
pub struct DocumentAssetManager {
    inference: Arc<dyn InferenceProvider>,
    store: Arc<dyn StateStore>,
    /// Optional local NER model for the T2 entity pass. See
    /// [`Self::with_entity_extractor`]. `None` keeps the LLM path.
    entity_extractor: Option<Arc<dyn EntityExtractor>>,
}

impl DocumentAssetManager {
    pub fn new(inference: Arc<dyn InferenceProvider>, store: Arc<dyn StateStore>) -> Self {
        Self {
            inference,
            store,
            entity_extractor: None,
        }
    }

    /// Use a local NER model for the T2 entity pass instead of the LLM.
    ///
    /// The window pass asks a 4B generative model to do one job — "list
    /// the named entities in this text" — which is what an NER model is
    /// for. On the 301-chunk bench subset that pass was ~50.9k of 77.6k
    /// total prompt tokens (66%), and token volume is what dominates
    /// ingest cost on a CPU-only host, where there is no idle batch
    /// capacity for scheduling tricks to harvest.
    ///
    /// Injected as `dyn EntityExtractor` rather than depending on
    /// `sovereign-gliner` directly: this crate stays free of the ONNX
    /// dependency, and hosts without the model installed simply don't
    /// call this. Extraction still degrades to the LLM per-window when
    /// the extractor returns nothing (see `build_skeleton`), so a
    /// not-yet-warm `LazyGlinerExtractor` cannot silently empty the
    /// skeleton.
    pub fn with_entity_extractor(mut self, extractor: Arc<dyn EntityExtractor>) -> Self {
        self.entity_extractor = Some(extractor);
        self
    }

    /// Parse + chunk + create the `Pending` asset record. No inference,
    /// so it's fast enough to call inline before returning to the UI.
    ///
    /// Returns a [`PreparedIngest`] whose `asset.id` is the id that
    /// [`run_ingest`](Self::run_ingest) will emit every progress event
    /// under. Callers that want to surface a live banner (the desktop
    /// upload command) call `prepare`, return `prepared.asset` to the
    /// UI, then spawn `run_ingest` — UI and events share one id.
    pub async fn prepare(&self, file_path: &std::path::Path) -> Result<PreparedIngest> {
        let parsed = parse_file(file_path)?;
        let filename = file_path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("document")
            .to_string();

        let text_chunks = chunk_text(&parsed.content);
        let word_count = parsed.content.split_whitespace().count();
        let chunk_count = text_chunks.len();
        let file_size_mb = std::fs::metadata(file_path)
            .map(|m| m.len() as f32 / (1024.0 * 1024.0))
            .unwrap_or(0.0);

        let asset_id = uuid::Uuid::new_v4().to_string();
        let index_id = format!("doc-{asset_id}");

        // Infer title from filename (strip extension).
        let title = file_path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or(&filename)
            .replace(['_', '-'], " ");

        let asset = DocumentAsset {
            id: asset_id,
            title,
            filename,
            file_size_mb,
            word_count,
            chunk_count,
            document_type: DocumentTypeTag::Unknown,
            ingested_at: chrono::Utc::now(),
            index_id,
            skeleton: None,
            state: AssetState::Pending,
            owner: None,
        };
        self.store.save_document_asset(&asset).await?;

        Ok(PreparedIngest { asset, text_chunks })
    }

    /// Parse, chunk, embed, and enrich a file in one call. Convenience
    /// wrapper over `prepare` + `run_ingest` for callers (server, CLI,
    /// tests) that wait for completion and don't need the id early.
    ///
    /// The progress callback fires at each phase boundary so the
    /// frontend can update the UI in real time.
    pub async fn ingest(
        &self,
        file_path: &std::path::Path,
        on_progress: impl Fn(IngestProgress) + Send + Sync + 'static,
    ) -> Result<DocumentAsset> {
        let prepared = self.prepare(file_path).await?;
        self.run_ingest(prepared, on_progress).await
    }

    /// Run the embed + tiered-enrichment pipeline on an already-prepared
    /// asset. Emits every `IngestProgress` under `prepared.asset.id`.
    /// Long-running: embeds all chunks then builds the RAPTOR atlas.
    pub async fn run_ingest(
        &self,
        prepared: PreparedIngest,
        on_progress: impl Fn(IngestProgress) + Send + Sync + 'static,
    ) -> Result<DocumentAsset> {
        let PreparedIngest { asset, text_chunks } = prepared;
        let on_progress: Arc<dyn Fn(IngestProgress) + Send + Sync> = Arc::new(on_progress);

        let asset_id = asset.id.clone();
        let filename = asset.filename.clone();
        let word_count = asset.word_count;
        let chunk_count = asset.chunk_count;
        let file_size_mb = asset.file_size_mb;
        let index_id = asset.index_id.clone();

        on_progress(IngestProgress::Started {
            word_count,
            chunk_count,
            filename: filename.clone(),
            asset_id: asset_id.clone(),
        });

        // ── Concurrent: embedding + skeleton ────────────────
        //
        // Embedding and skeleton extraction run in parallel via
        // tokio::join!. Embedding uses batch calls for throughput.
        // Once embedding finishes, RAG queries work even while the
        // skeleton is still building.

        let source_id = format!("asset:{asset_id}");
        let text_chunks = Arc::new(text_chunks);

        // ── Embedding future ────────────────────────────────
        let embed_future = {
            let inference = Arc::clone(&self.inference);
            let store = Arc::clone(&self.store);
            let asset_id = asset_id.clone();
            let source_id = source_id.clone();
            let text_chunks = Arc::clone(&text_chunks);
            let on_progress = Arc::clone(&on_progress);

            async move {
                store
                    .update_asset_state(
                        &asset_id,
                        &AssetState::Indexing {
                            chunks_done: 0,
                            chunks_total: chunk_count,
                        },
                    )
                    .await?;
                // Emit at 0% *before* the first batch so the banner flips
                // off "Queued…" the instant embedding starts — the embed
                // slot's lazy model load (tens of seconds, cold) lands in
                // this window, and without a 0% tick the UI looks frozen.
                on_progress(IngestProgress::Indexing {
                    done: 0,
                    total: chunk_count,
                });

                let now_ts = chrono::Utc::now().timestamp();
                let mut doc_chunks = Vec::with_capacity(chunk_count);

                // Batch embed in groups of 64 for throughput.
                const EMBED_BATCH: usize = 64;
                let mut all_embeddings: Vec<Option<Vec<f32>>> = Vec::with_capacity(chunk_count);

                for batch_start in (0..chunk_count).step_by(EMBED_BATCH) {
                    let batch_end = (batch_start + EMBED_BATCH).min(chunk_count);
                    let texts: Vec<String> = text_chunks[batch_start..batch_end]
                        .iter()
                        .map(|c| c.content.clone())
                        .collect();

                    match inference.embed_batch(&texts).await {
                        Ok(embeddings) => {
                            for emb in embeddings {
                                all_embeddings.push(Some(emb));
                            }
                        }
                        Err(_) => {
                            // Fallback: mark these as no-embedding.
                            for _ in batch_start..batch_end {
                                all_embeddings.push(None);
                            }
                        }
                    }

                    on_progress(IngestProgress::Indexing {
                        done: batch_end,
                        total: chunk_count,
                    });
                    let _ = store
                        .update_asset_state(
                            &asset_id,
                            &AssetState::Indexing {
                                chunks_done: batch_end,
                                chunks_total: chunk_count,
                            },
                        )
                        .await;
                }

                // Build DocumentChunk records.
                for (i, tc) in text_chunks.iter().enumerate() {
                    doc_chunks.push(DocumentChunk {
                        id: format!("{source_id}:{}", tc.index),
                        source: source_id.clone(),
                        content: tc.content.clone(),
                        chunk_index: tc.index,
                        embedding: all_embeddings.get(i).cloned().flatten(),
                        created_at: now_ts,
                        source_type: SourceType::UserDocument,
                        version: 0,
                        deleted_at: None,
                    });
                }

                store.store_chunks(&doc_chunks).await?;

                // RAG is now available.
                store
                    .update_asset_state(&asset_id, &AssetState::PartiallyReady)
                    .await?;
                on_progress(IngestProgress::RagAvailable {
                    asset_id: asset_id.clone(),
                });

                Ok::<(), sovereign_core::error::Error>(())
            }
        };

        // ── Tiered enrichment future (T2 → MultiHopReady → T3) ──
        //
        // Splits the prior monolithic skeleton phase into the two
        // tiered states defined in the proper-curried-peach plan:
        //
        //   T2: lean entity extraction + action atoms — yields a
        //       partial skeleton (entity_index + main_entities +
        //       actions + sections + structural_moments). Asset
        //       transitions to MultiHopReady. PPR multi-hop
        //       retrieval becomes available at this point.
        //
        //   T3: TextTiling segments + RAPTOR atlas + motif index +
        //       overview generation — fills in the remaining
        //       skeleton fields (segments, overview) AND populates
        //       the raptor_nodes + asset_motifs tables. Asset
        //       transitions to Ready. Full briefing-driven synthesis
        //       becomes available.
        //
        // Both phases run in the SAME future (not parallel with
        // embedding — they depend on chunks being persisted). The
        // foreground T3 is a deliberate change from the prior
        // background spawn: we want Ready to actually mean "all
        // enrichment landed," not "skeleton landed and RAPTOR is
        // still cooking."
        let skeleton_future = {
            let inference = Arc::clone(&self.inference);
            let store = Arc::clone(&self.store);
            let asset_id = asset_id.clone();
            let text_chunks = Arc::clone(&text_chunks);
            let on_progress = Arc::clone(&on_progress);
            let entity_extractor = self.entity_extractor.clone();

            async move {
                let doc_type = detect_document_type(&inference, &text_chunks).await;

                // ── T2 — entity extraction + action atoms ──────
                store
                    .update_asset_state(
                        &asset_id,
                        &AssetState::BuildingSkeleton {
                            chunks_done: 0,
                            chunks_total: chunk_count,
                        },
                    )
                    .await?;

                let mut skeleton = build_skeleton(
                    &inference,
                    &store,
                    &asset_id,
                    &text_chunks,
                    &doc_type,
                    &on_progress,
                    entity_extractor.as_ref(),
                )
                .await?;

                // Persist partial skeleton + transition to
                // MultiHopReady so queries arriving in the
                // T3-window can use PPR.
                store
                    .save_asset_skeleton(&asset_id, &skeleton, &doc_type)
                    .await?;
                store
                    .update_asset_state(&asset_id, &AssetState::MultiHopReady)
                    .await?;
                on_progress(IngestProgress::MultiHopReady {
                    asset_id: asset_id.clone(),
                });

                // ── T3 — RAPTOR atlas + motifs + segments + overview ──
                // Re-emit BuildingSkeleton state so the UI's progress
                // bar reactivates for the T3 enrichment phase. The
                // chunks_done counter resets at this milestone — by
                // design (the visual reset signals a real capability
                // checkpoint, not just continuous work).
                store
                    .update_asset_state(
                        &asset_id,
                        &AssetState::BuildingSkeleton {
                            chunks_done: 0,
                            chunks_total: chunk_count,
                        },
                    )
                    .await?;

                // Segments (TextTiling) + overview run concurrently —
                // both touch all chunks, neither depends on the other.
                // T1's persisted per-chunk embeddings are fetched and
                // reused for TextTiling (same model, same texts) — the
                // fetch is a local store read, the re-embed it replaces
                // was ~30s of embed-slot time per 300 chunks.
                let stored_embeddings: Option<Vec<Vec<f32>>> = {
                    let source_key = format!("asset:{asset_id}");
                    match store.get_chunks_by_source(&source_key).await {
                        Ok(mut docs) if docs.len() == text_chunks.len() => {
                            docs.sort_by_key(|d| d.chunk_index);
                            let embs: Vec<Vec<f32>> =
                                docs.into_iter().filter_map(|d| d.embedding).collect();
                            (embs.len() == text_chunks.len()).then_some(embs)
                        }
                        _ => None,
                    }
                };
                let segments_future = extract_segments(
                    &inference,
                    &text_chunks,
                    &skeleton.main_entities,
                    doc_type.clone(),
                    stored_embeddings,
                );
                let overview_future = generate_overview(&inference, &text_chunks, &doc_type);
                let (segments, overview) = tokio::join!(segments_future, overview_future);
                skeleton.segments = segments;
                skeleton.overview = overview;
                // Coarse progress checkpoint after segments+overview —
                // small fraction of T3 wall-clock but worth a tick so
                // the UI doesn't look frozen during the embedding
                // window of TextTiling + the single overview LLM call.
                on_progress(IngestProgress::BuildingSkeleton {
                    done: (chunk_count as f32 * 0.10).round() as usize,
                    total: chunk_count,
                });
                let _ = store
                    .update_asset_state(
                        &asset_id,
                        &AssetState::BuildingSkeleton {
                            chunks_done: (chunk_count as f32 * 0.10).round() as usize,
                            chunks_total: chunk_count,
                        },
                    )
                    .await;

                // RAPTOR + motif build. Failures are logged inside
                // the helper and degrade quality without breaking
                // ingest — the partial skeleton from T2 is still a
                // valid retrieval surface. Progress events fire at
                // coarse phase boundaries inside this helper so the
                // UI bar continues to advance through the ~4-min
                // window.
                let source_key = format!("asset:{asset_id}");
                build_and_persist_raptor_atlas(
                    &inference,
                    &store,
                    &asset_id,
                    &source_key,
                    doc_type.clone(),
                    &on_progress,
                    chunk_count,
                )
                .await;

                // Final skeleton save (with overview + segments now
                // populated) + Ready transition.
                store
                    .save_asset_skeleton(&asset_id, &skeleton, &doc_type)
                    .await?;
                store
                    .update_asset_state(&asset_id, &AssetState::Ready)
                    .await?;
                on_progress(IngestProgress::Ready {
                    asset_id: asset_id.clone(),
                    main_entities: skeleton.main_entities.len(),
                    structural_moments: skeleton.structural_moments.len(),
                });

                Ok::<(DocumentSkeleton, DocumentTypeTag), sovereign_core::error::Error>((
                    skeleton, doc_type,
                ))
            }
        };

        // Embedding + tiered enrichment run concurrently. Embedding
        // typically finishes first (pure computation), flipping the
        // asset to PartiallyReady (T1 done) so cosine retrieval works
        // immediately. The enrichment future then walks T2 → T3.
        let (embed_result, skeleton_result) = tokio::join!(embed_future, skeleton_future);

        embed_result?;
        let (skeleton, doc_type) = skeleton_result?;

        // (T3 used to be a tokio::spawn background task here. It
        // now runs inside skeleton_future above, so Ready means
        // *all* enrichment has landed.)

        Ok(DocumentAsset {
            id: asset_id,
            title: asset.title,
            filename,
            file_size_mb,
            word_count,
            chunk_count,
            document_type: doc_type,
            ingested_at: asset.ingested_at,
            index_id,
            skeleton: Some(skeleton),
            state: AssetState::Ready,
            owner: None,
        })
    }

    /// Rebuild the skeleton for an already-ingested asset, working entirely
    /// from stored chunks — no file path required, no re-parsing, no
    /// re-embedding.
    ///
    /// Used two ways:
    /// 1. The `rebuild_document_skeleton` Tauri command (user-initiated).
    /// 2. Auto-heal: when `ask_document` sees a skeleton-less asset, it
    ///    spawns this in the background so subsequent turns get smarter
    ///    routing without the user doing anything.
    ///
    /// Returns the freshly-built skeleton; the asset's stored skeleton and
    /// `document_type` are updated atomically via `save_asset_skeleton`, and
    /// the asset state transitions to `Ready` on success.
    pub async fn rebuild_skeleton(&self, asset_id: &str) -> Result<DocumentSkeleton> {
        tracing::info!(asset_id = %asset_id, "rebuild_skeleton — begin");

        let asset = self
            .store
            .get_document_asset(asset_id)
            .await?
            .ok_or_else(|| Error::NotFound(format!("document asset {asset_id}")))?;

        let source_id = asset.source_key();
        let mut doc_chunks = self.store.get_chunks_by_source(&source_id).await?;

        if doc_chunks.is_empty() {
            tracing::warn!(
                asset_id = %asset_id,
                source_id = %source_id,
                "rebuild_skeleton — no chunks found; cannot rebuild"
            );
            return Err(Error::NotFound(format!(
                "no chunks for document asset {asset_id} — needs re-ingest from source file"
            )));
        }

        // DocumentChunks come back in insertion order but we want them
        // ordered by chunk_index so the skeleton batches reflect the
        // document's narrative order.
        doc_chunks.sort_by_key(|c| c.chunk_index);

        let text_chunks: Vec<TextChunk> = doc_chunks
            .into_iter()
            .map(|c| TextChunk {
                index: c.chunk_index,
                content: c.content,
            })
            .collect();
        let chunk_count = text_chunks.len();

        tracing::debug!(
            asset_id = %asset_id,
            chunks = chunk_count,
            "rebuild_skeleton — chunks loaded from store"
        );

        self.store
            .update_asset_state(
                asset_id,
                &AssetState::BuildingSkeleton {
                    chunks_done: 0,
                    chunks_total: chunk_count,
                },
            )
            .await?;

        let doc_type = detect_document_type(&self.inference, &text_chunks).await;

        // No UI progress on rebuilds — state updates inside build_skeleton
        // are the only signal. Callers who want per-batch feedback should
        // run a full re-ingest.
        let noop_progress: Arc<dyn Fn(IngestProgress) + Send + Sync> = Arc::new(|_| ());

        let skeleton = build_skeleton(
            &self.inference,
            &self.store,
            asset_id,
            &text_chunks,
            &doc_type,
            &noop_progress,
            self.entity_extractor.as_ref(),
        )
        .await?;

        self.store
            .save_asset_skeleton(asset_id, &skeleton, &doc_type)
            .await?;
        self.store
            .update_asset_state(asset_id, &AssetState::Ready)
            .await?;

        tracing::info!(
            asset_id = %asset_id,
            doc_type = ?doc_type,
            sections = skeleton.sections.len(),
            entities = skeleton.main_entities.len(),
            "rebuild_skeleton — done"
        );

        Ok(skeleton)
    }

    /// Rebuild ONLY the RAPTOR atlas + motif index for an existing
    /// Ready asset, leaving the legacy skeleton untouched. Used by
    /// the bench (`--rebuild-raptor`) and by future admin paths to
    /// populate the new atlas on documents ingested before the
    /// RAPTOR pipeline shipped — without paying for a full ~20-min
    /// skeleton rebuild.
    ///
    /// Returns `Ok(())` on success. Errors propagate from the store
    /// (no chunks, write failure) or the inference layer (embed /
    /// summarize failures). Per `build_and_persist_raptor_atlas`'s
    /// own contract, internal failures inside RAPTOR or motif
    /// extraction are logged + swallowed; this entry point only
    /// errors on upfront preconditions.
    pub async fn rebuild_raptor_atlas(&self, asset_id: &str) -> Result<()> {
        tracing::info!(asset_id = %asset_id, "rebuild_raptor_atlas — begin");
        let asset = self
            .store
            .get_document_asset(asset_id)
            .await?
            .ok_or_else(|| Error::NotFound(format!("document asset {asset_id}")))?;
        let source_key = asset.source_key();
        let doc_type = asset.document_type.clone();
        let chunk_count = asset.chunk_count;
        // Rebuild path has no UI progress channel — supply a noop
        // callback so the helper's signature can stay uniform with
        // the main ingest path.
        let noop_progress: Arc<dyn Fn(IngestProgress) + Send + Sync> = Arc::new(|_| ());
        build_and_persist_raptor_atlas(
            &self.inference,
            &self.store,
            asset_id,
            &source_key,
            doc_type,
            &noop_progress,
            chunk_count,
        )
        .await;
        Ok(())
    }

    /// Route a user's question to the right operation type, then
    /// execute it and return the response with source citations.
    pub async fn ask(
        &self,
        asset: &DocumentAsset,
        request: &str,
        on_progress: impl Fn(OperationProgress) + Send + Sync,
    ) -> Result<(String, DocumentAssetOperation, Vec<String>)> {
        let start = std::time::Instant::now();
        tracing::info!(
            asset_id = %asset.id,
            title = %asset.title,
            doc_type = ?asset.document_type,
            has_skeleton = asset.skeleton.is_some(),
            request_chars = request.len(),
            "DocumentAssetManager::ask — begin"
        );

        let operation = self.route(asset, request).await?;

        tracing::info!(
            asset_id = %asset.id,
            operation = %operation.label(),
            "DocumentAssetManager::ask — routed"
        );

        on_progress(OperationProgress::Routing {
            operation: operation.label().to_string(),
        });

        let output = self
            .execute_operation(asset, request, &operation, &on_progress)
            .await?;

        tracing::info!(
            asset_id = %asset.id,
            operation = %operation.label(),
            response_chars = output.text.len(),
            source_count = output.citations.len(),
            total_latency_ms = start.elapsed().as_millis() as u64,
            "DocumentAssetManager::ask — done"
        );

        // `ask()` stays on its old 3-tuple API for HTTP callers that only
        // need raw content strings. Tauri callers use `execute_operation`
        // directly and get the full `ExecutionOutput`.
        let sources: Vec<String> = output.citations.iter().map(|c| c.content.clone()).collect();

        Ok((output.text, operation, sources))
    }

    /// Delete an asset and its chunks.
    pub async fn delete(&self, id: &str) -> Result<()> {
        // Delete chunks from the document store.
        let source_id = format!("asset:{}", id);
        if let Ok(chunks) = self.store.get_chunks_by_source(&source_id).await {
            if !chunks.is_empty() {
                // Soft-delete by overwriting with empty + deleted_at.
                // The store's delete_chunks_by_corpus doesn't apply here
                // since these are UserDocument source type.
                // For now, we just delete the asset record — chunks are
                // orphaned but small. A future cleanup job can GC them.
            }
        }
        self.store.delete_document_asset(id).await
    }

    // ─── Routing ─────────────────────────────────────────────

    /// Classify a question into an operation type using the document's
    /// skeleton overview and the question text. Uses the fast model
    /// for low latency.
    ///
    /// Public so callers (the `ask_document` Tauri command) can inspect the
    /// routing decision before executing. In particular, when the router
    /// returns `OffTopic`, the caller can route the question through the
    /// normal conversation pipeline instead of the document operation path.
    pub async fn route(
        &self,
        asset: &DocumentAsset,
        request: &str,
    ) -> Result<DocumentAssetOperation> {
        tracing::debug!(asset_id = %asset.id, "document_asset::route — begin");

        // Deterministic pre-check: if the question explicitly references the
        // attached document ("this document", "summarize this paper", etc.)
        // we don't need an LLM to tell us the user wants a document answer.
        // Skip the Fast-slot call and go straight to Synthesis — which works
        // whether or not the skeleton has been built (execute_synthesis
        // samples chunks evenly when the skeleton is absent).
        //
        // Without this check, a skeleton-less asset would have a placeholder
        // overview ("Document structure not yet available.") and the Fast
        // classifier would often default to off_topic even for clearly
        // document-directed questions like "summarize this document".
        if detect_self_reference(request) {
            tracing::info!(
                asset_id = %asset.id,
                "document_asset::route — self-reference detected, defaulting to Synthesis"
            );
            return Ok(DocumentAssetOperation::Synthesis {
                focus: request.to_string(),
                entities: Vec::new(),
            });
        }

        // Filename / title grounding: if the question mentions a content
        // word from the document's filename or title (author name, key
        // concept, etc.), the user is almost certainly asking about this
        // document. Route to Synthesis without a Fast-slot classification
        // call — more reliable than depending on the model's judgment when
        // the skeleton isn't built yet.
        let tokens = filename_tokens(asset);
        if mentions_document(&tokens, request) {
            tracing::info!(
                asset_id = %asset.id,
                tokens = ?tokens.iter().take(5).collect::<Vec<_>>(),
                "document_asset::route — filename grounding matched, defaulting to Synthesis"
            );
            return Ok(DocumentAssetOperation::Synthesis {
                focus: request.to_string(),
                entities: Vec::new(),
            });
        }

        let overview = asset
            .skeleton
            .as_ref()
            .map(|s| s.overview.as_str())
            .unwrap_or("Document structure not yet available.");

        let entity_names: Vec<String> = asset
            .skeleton
            .as_ref()
            .map(|s| s.main_entities.iter().map(|e| e.name.clone()).collect())
            .unwrap_or_default();

        tracing::debug!(
            overview_chars = overview.len(),
            entity_count = entity_names.len(),
            "document_asset::route — classifying"
        );

        let prompt = format!(
            "You are a document operation router. Given a user's question about a document, \
             classify it into exactly one operation type.\n\n\
             Document overview: {overview}\n\
             Main entities: {entities}\n\
             Document type: {doc_type}\n\n\
             User question: {request}\n\n\
             Respond with exactly one of these JSON objects:\n\
             - {{\"op\": \"rag\", \"query\": \"<search query>\"}}\n\
             - {{\"op\": \"synthesis\", \"focus\": \"<what to trace>\", \"entities\": [\"<names>\"]}}\n\
             - {{\"op\": \"aggregation\", \"query\": \"<what to find all of>\"}}\n\
             - {{\"op\": \"transformation\"}}\n\
             - {{\"op\": \"off_topic\", \"reason\": \"<brief why>\"}}\n\n\
             Guidelines:\n\
             - Use \"rag\" for questions about specific passages, chapters, or facts \
               in THIS document.\n\
             - Use \"synthesis\" for questions that require tracing something across \
               the full document (character arcs, argument development, thematic evolution).\n\
             - Use \"aggregation\" for \"find every mention of X\" or \"list all instances of Y\".\n\
             - Use \"transformation\" for rewriting, editing, or extracting structured data.\n\
             - Use \"off_topic\" when the question is clearly about a different \
               domain AND makes no reference to the attached document — for \
               example, the document is about physics and the user asks about \
               Buddhism without mentioning the document. A question that says \
               \"this document\", \"this text\", \"the paper\", \"summarize this\", \
               or similar self-referential phrasing is NEVER off_topic.\n\
             - When you're unsure whether the topic is in the document, prefer \
               \"synthesis\" over \"off_topic\". Synthesis still works when the \
               document hasn't been fully analysed yet, so it's the safer default.\n\n\
             Respond with only the JSON object, no other text.",
            entities = entity_names.join(", "),
            doc_type = asset.document_type.label(),
        );

        // SLOT_POLICY §3 Route: operation classification consumed by
        // parse_route_response (control flow), never shown to the user.
        // Route's Some(0) think budget matches this site verbatim.
        let mut req = Workload::Route.request(prompt).with_output_budget(128);
        req.temperature = Some(0.0);
        let response = self.inference.complete(&req).await?;

        parse_route_response(&response.text, request)
    }

    // ─── Execution ───────────────────────────────────────────

    /// Execute a routed operation against the document.
    ///
    /// Public so the `ask_document` Tauri command can orchestrate
    /// `route` + `execute_operation` and branch to the runtime conversation
    /// pipeline when the routing decision is `OffTopic` or RAG retrieval
    /// comes up empty.
    pub async fn execute_operation(
        &self,
        asset: &DocumentAsset,
        request: &str,
        operation: &DocumentAssetOperation,
        on_progress: &(dyn Fn(OperationProgress) + Send + Sync),
    ) -> Result<ExecutionOutput> {
        let source_id = asset.source_key();

        match operation {
            DocumentAssetOperation::Rag { query } => {
                on_progress(OperationProgress::Retrieving);
                self.execute_rag(&source_id, query, request).await
            }
            DocumentAssetOperation::Synthesis { focus, entities } => {
                for entity in entities {
                    on_progress(OperationProgress::AnalysingEntity {
                        name: entity.clone(),
                    });
                }
                on_progress(OperationProgress::Synthesising);
                self.execute_synthesis(asset, focus, entities, request)
                    .await
            }
            DocumentAssetOperation::Aggregation { query } => {
                on_progress(OperationProgress::Retrieving);
                self.execute_aggregation(&source_id, query, request).await
            }
            DocumentAssetOperation::Transformation => {
                on_progress(OperationProgress::Synthesising);
                self.execute_transformation(&source_id, request).await
            }
            DocumentAssetOperation::OffTopic { .. } => {
                // The manager never executes OffTopic itself — the Tauri
                // handler is expected to detect it via the public `route()`
                // method and route the question through the normal
                // conversation pipeline (which gets corpus search, layered
                // confidence synthesis, etc.).
                //
                // Reaching this arm means a caller bypassed the pre-check
                // and called `ask()` with an OffTopic operation; return a
                // sentinel so the behavior is at least well-defined.
                Err(Error::Execution(
                    "OffTopic must be handled by the caller via runtime.handle_turn".into(),
                ))
            }
        }
    }

    /// RAG: retrieve relevant passages and synthesise an answer.
    ///
    /// When retrieval returns zero document-matching chunks this method
    /// returns an empty response + empty sources as a signal that the
    /// question wasn't really about the document. The caller (the
    /// `ask_document` Tauri command) detects the empty sources and falls
    /// through to the normal conversation pipeline.
    async fn execute_rag(
        &self,
        source_id: &str,
        query: &str,
        original_request: &str,
    ) -> Result<ExecutionOutput> {
        tracing::info!(
            source_id = %source_id,
            query_chars = query.len(),
            "execute_rag — begin"
        );

        let query_embedding = self.inference.embed(query).await?;
        let results = self
            .store
            .search_documents(&query_embedding, query, 8)
            .await?;

        // Filter to chunks from this document only.
        let relevant: Vec<&DocumentChunk> =
            results.iter().filter(|c| c.source == source_id).collect();

        tracing::debug!(
            total_results = results.len(),
            relevant_count = relevant.len(),
            "execute_rag — retrieval done"
        );

        if relevant.is_empty() {
            // Empty sources signal to the Tauri handler that this turn should
            // fall through to the normal conversation pipeline (corpus search,
            // layered confidence synthesis). The router ideally classifies
            // such questions as OffTopic up front; this is the safety net.
            tracing::warn!(
                source_id = %source_id,
                "execute_rag — no relevant passages; caller should fall back to runtime"
            );
            return Ok(ExecutionOutput::empty());
        }

        // Build labeled passages. Each citation label ("passage 1") is what
        // the model will emit as [Source: passage 1] in its answer, and also
        // what the frontend matches against `retrieved_chunks[].title` when
        // rendering popovers.
        let citations: Vec<CitedChunk> = relevant
            .iter()
            .enumerate()
            .map(|(i, c)| CitedChunk {
                label: format!("passage {}", i + 1),
                chunk_index: c.chunk_index,
                snippet: short_snippet(&c.content, 200),
                content: c.content.clone(),
            })
            .collect();

        let passages: String = citations
            .iter()
            .map(|c| format!("[Source: {}] {}", c.label, c.content))
            .collect::<Vec<_>>()
            .join("\n\n");

        let prompt = format!(
            "Answer the user's question based on these passages from the document.\n\n\
             Passages:\n{passages}\n\n\
             Question: {original_request}\n\n\
             Cite using [Source: passage N] notation — matching the labels above — \
             when referencing specific content. If the passages don't contain \
             enough information, say so honestly."
        );

        let response = self
            .inference
            .complete(&CompletionRequest {
                prompt,
                system_message: None,
                preferred_speed: Speed::Slow,
                max_tokens: Some(1024),
                temperature: Some(0.3),
                think_budget: Some(0),
                structured_output: None,
                top_k: None,
                top_p: None,
                oicp: None,
                tools: None,
                tool_choice: None,
                model_id: None,
                enable_thinking: None,
                sampling_mode: None,
                assistant_prefix: None,
                cmd_prefix: None,
                url_allowlist: None,
                evidence_id_allowlist: None,
                lark_grammar: None,
                prompt_shape: None,
                stable_prefix_len: None,
            })
            .await?;

        Ok(ExecutionOutput {
            text: response.text,
            citations,
            model_id: response.model_id,
            tokens_used: response.tokens_used,
            finish_reason: response.finish_reason.clone(),
            completion_tokens: response.completion_tokens,
            latency_ms: response.latency_ms,
        })
    }

    /// Synthesis: trace an entity or theme across the full document
    /// using the skeleton's entity index.
    async fn execute_synthesis(
        &self,
        asset: &DocumentAsset,
        focus: &str,
        entities: &[String],
        original_request: &str,
    ) -> Result<ExecutionOutput> {
        tracing::info!(
            asset_id = %asset.id,
            focus_chars = focus.len(),
            entity_count = entities.len(),
            "execute_synthesis — begin"
        );

        let source_id = asset.source_key();
        let all_chunks = self.store.get_chunks_by_source(&source_id).await?;

        tracing::debug!(
            total_chunks = all_chunks.len(),
            has_skeleton = asset.skeleton.is_some(),
            "execute_synthesis — chunks loaded"
        );

        if all_chunks.is_empty() {
            tracing::warn!(asset_id = %asset.id, "execute_synthesis — document has no indexed content");
            return Ok(ExecutionOutput {
                text: "Document has no indexed content.".to_string(),
                citations: Vec::new(),
                model_id: String::new(),
                tokens_used: 0,
                finish_reason: None,
                completion_tokens: None,
                latency_ms: 0,
            });
        }

        // Use the skeleton entity index to find relevant chunk indices.
        let relevant_indices: Vec<usize> = if let Some(ref skeleton) = asset.skeleton {
            let mut indices = Vec::new();
            for entity_name in entities {
                if let Some(appearances) = skeleton.entity_index.get(entity_name) {
                    indices.extend(&appearances.chunk_indices);
                }
            }
            indices.sort();
            indices.dedup();
            if indices.is_empty() {
                // Fallback: sample evenly across the document. The
                // `.max(1)` must wrap the DIVISION — `len.max(1) / 20`
                // is 0 for any document under 20 chunks, and
                // `step_by(0)` panics (caught 2026-06-09 by the
                // real-mode e2e: ask_document on a 2-chunk note
                // killed the worker mid-request).
                (0..all_chunks.len())
                    .step_by((all_chunks.len() / 20).max(1))
                    .collect()
            } else {
                indices
            }
        } else {
            // No skeleton — degrade to sampling.
            (0..all_chunks.len())
                .step_by((all_chunks.len() / 20).max(1))
                .collect()
        };

        let selected: Vec<&DocumentChunk> = relevant_indices
            .iter()
            .filter_map(|&i| all_chunks.get(i))
            .take(30) // Cap to avoid prompt overflow.
            .collect();

        tracing::debug!(
            selected_count = selected.len(),
            relevant_indices_count = relevant_indices.len(),
            "execute_synthesis — chunks selected"
        );

        // Build citation metadata alongside the prompt. Each chunk gets a
        // label `§<chunk_index>` that serves as both the prompt marker
        // AND the `title` the frontend matches against when rendering
        // [Source: §N] popovers.
        let citations: Vec<CitedChunk> = selected
            .iter()
            .map(|c| {
                let truncated = short_snippet(&c.content, 500);
                CitedChunk {
                    label: format!("§{}", c.chunk_index),
                    chunk_index: c.chunk_index,
                    snippet: short_snippet(&c.content, 200),
                    content: truncated, // prompt-sized copy
                }
            })
            .collect();

        let passages: String = citations
            .iter()
            .map(|c| format!("[Source: {}] {}", c.label, c.content))
            .collect::<Vec<_>>()
            .join("\n\n");

        // Keep the full content around for the assistant-message's
        // `sources` field (legacy UI), separate from the prompt-trimmed
        // text inside `citations`.
        let full_sources: Vec<String> = selected.iter().map(|c| c.content.clone()).collect();

        let skeleton_context = asset
            .skeleton
            .as_ref()
            .map(|s| {
                let moments: String = s
                    .structural_moments
                    .iter()
                    .take(10)
                    .map(|m| format!("- §{}: {}", m.chunk_index, m.description))
                    .collect::<Vec<_>>()
                    .join("\n");
                format!(
                    "Document overview: {}\n\nKey structural moments:\n{}",
                    s.overview, moments
                )
            })
            .unwrap_or_default();

        let prompt = format!(
            "You are analysing a full document. Synthesise an answer that traces \
             how {focus} develops across the text.\n\n\
             {skeleton_context}\n\n\
             Relevant sections (in document order):\n{passages}\n\n\
             Question: {original_request}\n\n\
             Draw on observations from early, middle, and late sections. \
             Cite sections using [Source: §N] notation — use the exact \
             labels shown above (e.g. [Source: §4], [Source: §16]) when \
             referencing specific content."
        );

        // SLOT_POLICY §3 Synthesize: full-document synthesis composed for
        // the user (traces a focus across the text).
        let mut req = Workload::Synthesize
            .request(prompt)
            .with_output_budget(2048);
        req.temperature = Some(0.5);
        // POLICY-DEBT(SLOT_POLICY §3 Synthesize): Some(0) preserved for P1
        // neutrality (bundle is None); P5 confirms.
        req.think_budget = Some(0);
        let response = self.inference.complete(&req).await?;

        // Swap the prompt-trimmed `content` on each citation back out for
        // the full chunk content so the Tauri handler persists the real
        // source text alongside the snippet.
        let citations: Vec<CitedChunk> = citations
            .into_iter()
            .zip(full_sources)
            .map(|(mut c, full)| {
                c.content = full;
                c
            })
            .collect();

        Ok(ExecutionOutput {
            text: response.text,
            citations,
            model_id: response.model_id,
            tokens_used: response.tokens_used,
            finish_reason: response.finish_reason.clone(),
            completion_tokens: response.completion_tokens,
            latency_ms: response.latency_ms,
        })
    }

    /// Aggregation: search every section for all instances matching
    /// the query.
    async fn execute_aggregation(
        &self,
        source_id: &str,
        query: &str,
        original_request: &str,
    ) -> Result<ExecutionOutput> {
        tracing::info!(
            source_id = %source_id,
            query_chars = query.len(),
            "execute_aggregation — begin"
        );

        let all_chunks = self.store.get_chunks_by_source(source_id).await?;

        // Simple keyword/embedding scan over all chunks.
        let query_lower = query.to_lowercase();
        let matching: Vec<&DocumentChunk> = all_chunks
            .iter()
            .filter(|c| c.content.to_lowercase().contains(&query_lower))
            .collect();

        tracing::debug!(
            total_chunks = all_chunks.len(),
            matching_count = matching.len(),
            "execute_aggregation — keyword scan done"
        );

        if matching.is_empty() {
            tracing::warn!(query = %query, "execute_aggregation — no matches found");
            return Ok(ExecutionOutput {
                text: format!("No instances of \"{query}\" found in the document."),
                citations: Vec::new(),
                model_id: String::new(),
                tokens_used: 0,
                finish_reason: None,
                completion_tokens: None,
                latency_ms: 0,
            });
        }

        // Build citations for the first 50 matches. Each gets a label
        // `match N` that the model will cite as [Source: match N].
        let citations: Vec<CitedChunk> = matching
            .iter()
            .take(50)
            .enumerate()
            .map(|(i, c)| CitedChunk {
                label: format!("match {}", i + 1),
                chunk_index: c.chunk_index,
                snippet: short_snippet(&c.content, 200),
                content: c.content.clone(),
            })
            .collect();

        let matches_text: String = citations
            .iter()
            .map(|c| {
                let excerpt = short_snippet(&c.content, 300);
                format!(
                    "[Source: {}] §{}: ...{}...",
                    c.label, c.chunk_index, excerpt
                )
            })
            .collect::<Vec<_>>()
            .join("\n\n");

        let prompt = format!(
            "The user asked: {original_request}\n\n\
             Found {} instances across the document:\n\n{matches_text}\n\n\
             Summarise the findings. Group by theme or chronology if appropriate. \
             Cite using [Source: match N] notation — matching the labels above.",
            matching.len(),
        );

        let response = self
            .inference
            .complete(&CompletionRequest {
                prompt,
                system_message: None,
                preferred_speed: Speed::Slow,
                max_tokens: Some(1024),
                temperature: Some(0.3),
                think_budget: Some(0),
                structured_output: None,
                top_k: None,
                top_p: None,
                oicp: None,
                tools: None,
                tool_choice: None,
                model_id: None,
                enable_thinking: None,
                sampling_mode: None,
                assistant_prefix: None,
                cmd_prefix: None,
                url_allowlist: None,
                evidence_id_allowlist: None,
                lark_grammar: None,
                prompt_shape: None,
                stable_prefix_len: None,
            })
            .await?;

        Ok(ExecutionOutput {
            text: response.text,
            citations,
            model_id: response.model_id,
            tokens_used: response.tokens_used,
            finish_reason: response.finish_reason.clone(),
            completion_tokens: response.completion_tokens,
            latency_ms: response.latency_ms,
        })
    }

    /// Transformation: apply a user-requested transformation.
    async fn execute_transformation(
        &self,
        source_id: &str,
        request: &str,
    ) -> Result<ExecutionOutput> {
        tracing::info!(
            source_id = %source_id,
            request_chars = request.len(),
            "execute_transformation — begin"
        );

        let all_chunks = self.store.get_chunks_by_source(source_id).await?;
        let full_text: String = all_chunks
            .iter()
            .take(20) // Limit to avoid prompt overflow.
            .map(|c| c.content.as_str())
            .collect::<Vec<_>>()
            .join("\n\n");

        tracing::debug!(
            total_chunks = all_chunks.len(),
            used_chunks = all_chunks.len().min(20),
            full_text_chars = full_text.len(),
            "execute_transformation — text assembled"
        );

        let prompt = format!(
            "Apply the following transformation to the document text:\n\n\
             Transformation: {request}\n\n\
             Document:\n{full_text}"
        );

        let response = self
            .inference
            .complete(&CompletionRequest {
                prompt,
                system_message: None,
                preferred_speed: Speed::Slow,
                max_tokens: Some(2048),
                temperature: Some(0.3),
                think_budget: Some(0),
                structured_output: None,
                top_k: None,
                top_p: None,
                oicp: None,
                tools: None,
                tool_choice: None,
                model_id: None,
                enable_thinking: None,
                sampling_mode: None,
                assistant_prefix: None,
                cmd_prefix: None,
                url_allowlist: None,
                evidence_id_allowlist: None,
                lark_grammar: None,
                prompt_shape: None,
                stable_prefix_len: None,
            })
            .await?;

        // Transformations consume the whole document; we don't surface
        // per-chunk citations because the output is the transformed text,
        // not a referenced answer.
        Ok(ExecutionOutput {
            text: response.text,
            citations: Vec::new(),
            model_id: response.model_id,
            tokens_used: response.tokens_used,
            finish_reason: response.finish_reason.clone(),
            completion_tokens: response.completion_tokens,
            latency_ms: response.latency_ms,
        })
    }
}

// ─── Skeleton extraction (free functions) ────────────────────
//
// These are free functions rather than methods on DocumentAssetManager
// because they're called from spawned futures that can't borrow &self.

/// Detect the document type from the first few chunks.
async fn detect_document_type(
    inference: &Arc<dyn InferenceProvider>,
    chunks: &[TextChunk],
) -> DocumentTypeTag {
    let sample: String = chunks
        .iter()
        .take(3)
        .map(|c| c.content.as_str())
        .collect::<Vec<_>>()
        .join("\n\n");

    let prompt = format!(
        "Classify this document into one category based on these opening passages:\n\n\
         {sample}\n\n\
         Categories:\n\
         - Narrative (novels, memoirs, literary non-fiction)\n\
         - Argument (dissertations, essays, philosophy)\n\
         - Evidence (legal briefs, scientific papers)\n\
         - Chronicle (history, biography, journalism)\n\
         - Technical (manuals, specifications, documentation)\n\n\
         Respond with exactly one word: Narrative, Argument, Evidence, Chronicle, or Technical."
    );

    // SLOT_POLICY §3 Route: document-type classification consumed by
    // control flow (DocumentTypeTag), never shown to the user. Route's
    // Some(0) think budget matches this site verbatim.
    let mut request = Workload::Route.request(prompt).with_output_budget(16);
    request.temperature = Some(0.0);
    let response = inference.complete(&request).await;

    let detected = match response {
        Ok(r) => {
            // Strip `<think>...</think>` blocks first — Qwen thinking
            // models emit them even when `think_budget: Some(0)` is set,
            // and without stripping the raw text looks like
            // `"<think>\n</think>\n\nArgument"` which never matches any
            // category and always falls through to Unknown.
            let cleaned = sovereign_core::title::strip_think_blocks(&r.text);
            match cleaned.trim().to_lowercase().as_str() {
                "narrative" => DocumentTypeTag::Narrative,
                "argument" => DocumentTypeTag::Argument,
                "evidence" => DocumentTypeTag::Evidence,
                "chronicle" => DocumentTypeTag::Chronicle,
                "technical" => DocumentTypeTag::Technical,
                _ => DocumentTypeTag::Unknown,
            }
        }
        Err(e) => {
            tracing::warn!(error = %e, "detect_document_type — inference failed, defaulting to Unknown");
            DocumentTypeTag::Unknown
        }
    };
    tracing::info!(detected = ?detected, "detect_document_type — classified");
    detected
}

/// Build the T2 (entity-tier) skeleton by processing chunks in
/// parallel through the LLM. Extracts sections, entities (with
/// kind), and structural moments. Returns a *partial* skeleton —
/// `overview` is empty and `segments` is empty; those are T3
/// outputs, filled in by `build_and_persist_raptor_atlas`.
///
/// Parallelism: per-batch tasks fan out across the mesh via
/// `futures::stream::iter(...).buffered(T2_BATCH_CONCURRENCY)`. On a
/// 2-peer mesh, this gives near-linear speedup over the previous
/// sequential `for batch in chunks.chunks(4)` loop; on a single-machine
/// deployment, the Slow slot serialises them but the async overhead
/// is no worse than the sequential version. The May-21 lean-grammar
/// probe measured 1.4s/batch; with concurrency=6 on a 250-batch
/// document that projects to ~60s for the entity-extraction phase.
async fn build_skeleton(
    inference: &Arc<dyn InferenceProvider>,
    store: &Arc<dyn StateStore>,
    asset_id: &str,
    chunks: &[TextChunk],
    doc_type: &DocumentTypeTag,
    on_progress: &Arc<dyn Fn(IngestProgress) + Send + Sync>,
    entity_extractor: Option<&Arc<dyn EntityExtractor>>,
) -> Result<DocumentSkeleton> {
    let chunk_count = chunks.len();

    // Glassbox: which T2 entity path is this ingest taking? A local NER
    // model (GLiNER) when one is wired, else the per-window LLM pass.
    // Per-window fallback (empty NER result) is logged at the call site;
    // this line records the intended path once, up front, so an operator
    // reading logs can see the −70%-token swap is engaged without
    // inferring it from the absence of "List the named entities" calls.
    tracing::info!(
        chunks = chunk_count,
        entity_path = if entity_extractor.is_some() {
            "ner"
        } else {
            "llm"
        },
        "build_skeleton — T2 entity extraction path"
    );

    // Process chunks in 12-chunk windows (was batches of 4 until
    // 2026-07-24). Profiling on the turbocharge arc showed DECODE
    // volume is the enrichment wall on this hardware (batched decode
    // doesn't amortize on Vulkan/LPDDR5), and per-chunk entity lines
    // re-emit the same recurring names ~N times per window. The window
    // schema emits ONE deduped name list per window (~3.5× less
    // decode, 3× fewer calls) and chunk-level attribution is
    // recovered DETERMINISTICALLY by scanning each window chunk for
    // each name (`parse_window_skeleton_batch`) — exact where the
    // model's per-line alignment was merely grammar-constrained.
    let batch_size = 12;
    let batches: Vec<(usize, Vec<TextChunk>)> = chunks
        .chunks(batch_size)
        .enumerate()
        .map(|(idx, b)| (idx, b.to_vec()))
        .collect();
    let total_batches = batches.len();
    let completed = Arc::new(AtomicUsize::new(0));

    let inference_arc = Arc::clone(inference);
    let store_arc = Arc::clone(store);
    let on_progress_arc = Arc::clone(on_progress);
    let asset_id_owned = asset_id.to_string();
    let doc_type_owned = doc_type.clone();
    let chunk_count_for_progress = chunk_count;

    let extractor_arc = entity_extractor.cloned();

    let batch_results: Vec<(usize, Option<Vec<SkeletonBatchEntry>>)> = stream::iter(batches)
        .map(|(batch_idx, batch)| {
            let inference = Arc::clone(&inference_arc);
            let store = Arc::clone(&store_arc);
            let on_progress = Arc::clone(&on_progress_arc);
            let asset_id = asset_id_owned.clone();
            let doc_type = doc_type_owned.clone();
            let completed = Arc::clone(&completed);
            let extractor = extractor_arc.clone();
            async move {
                let batch_start = batch_idx * batch_size;
                let passage: String = batch
                    .iter()
                    .map(|c| c.content.as_str())
                    .collect::<Vec<_>>()
                    .join("\n\n---\n\n");

                // ── NER fast path.
                //
                // The LLM prompt below asks for exactly one thing: the
                // named entities present in this window. A local NER
                // model answers that directly, for zero LLM tokens.
                //
                // `extract_entities` is synchronous and CPU-bound (ONNX),
                // so it goes on the blocking pool — running it inline
                // would stall the executor driving the other windows.
                //
                // An empty result is NOT trusted: `LazyGlinerExtractor`
                // returns empty until its background load finishes, and a
                // model that whiffs on a window would silently erase that
                // window's entities. Empty ⇒ fall through to the LLM, so
                // the worst case is the previous behaviour.
                let ner_names: Option<Vec<String>> = match extractor {
                    Some(g) => {
                        let text = passage.clone();
                        match tokio::task::spawn_blocking(move || g.extract_entities(&text)).await {
                            Ok(names) if !names.is_empty() => Some(names),
                            Ok(_) => None,
                            Err(e) => {
                                tracing::debug!(
                                    batch_idx,
                                    error = %e,
                                    "build_skeleton — NER task failed; falling back to LLM for this window"
                                );
                                None
                            }
                        }
                    }
                    None => None,
                };
                if let Some(names) = ner_names {
                    let entries = attribute_entity_names(names, batch_start, &batch);
                    let done_now = completed.fetch_add(1, Ordering::SeqCst) + 1;
                    let chunks_done = (done_now * batch_size).min(chunk_count_for_progress);
                    on_progress(IngestProgress::BuildingSkeleton {
                        done: chunks_done,
                        total: chunk_count_for_progress,
                    });
                    let _ = store
                        .update_asset_state(
                            &asset_id,
                            &AssetState::BuildingSkeleton {
                                chunks_done,
                                chunks_total: chunk_count_for_progress,
                            },
                        )
                        .await;
                    return (batch_idx, Some(entries));
                }

                // Window entity schema with llguidance grammar
                // enforcement: ONE deduped, comma-separated list of
                // canonical names for the whole window. Chunk-level
                // attribution happens in the parser by scanning each
                // window chunk for each name — the model only has to
                // NAME the entities, never to align them.
                let prompt = format!(
                    "List the named entities mentioned in the passage below from this \
                     {doc_type} document — characters, organizations, places, key \
                     concepts — using their canonical names EXACTLY as they appear in \
                     the text. Output ONE comma-separated list with each name once. \
                     No prose, no JSON, no headers.\n\n\
                     Passage (sections from {batch_start}):\n\n{passage}\n\nAnswer (one line):",
                    doc_type = doc_type.label(),
                );
                let lark_grammar = "start: line\n\
                     line: (entity (\",\" \" \"? entity)*)?\n\
                     entity: /[A-Z][A-Za-z'.]*( [A-Z][A-Za-z'.]*)*/\n"
                    .to_string();

                // SLOT_POLICY §3 EnrichBulk: high-volume, small-output,
                // grammar-constrained extraction — the Fast-class bundle
                // whose 512-token cap this fits with 4× headroom.
                // Changed from ExtractDurable 2026-07-24 (enrichment
                // turbocharge arc): Normal-class routing serialized all
                // ~250 batches through the single primary slot, making
                // `buffered(N)` fan-out a no-op locally; Fast-class
                // routing engages the FastShort continuous-batching
                // companion under fan-out, which is REAL concurrency.
                // Durability is protected by the llguidance grammar
                // (shape cannot desync) + the 4B-parity result from the
                // 2026-07-23 enrichment-model ladder (skeleton quality
                // is not model-bound above 4B).
                let mut request =
                    Workload::EnrichBulk.request(prompt)
                        .with_output_budget(120);
                request.temperature = Some(0.1);
                // Grammar constraint preserved verbatim (see lark_grammar above).
                request.lark_grammar = Some(lark_grammar);
                // POLICY-DEBT(SLOT_POLICY §3 ExtractDurable): Some(0) preserved
                // for P1 neutrality (bundle is None); P5 confirms.
                request.think_budget = Some(0);
                let response = inference.complete(&request).await;
                let parsed = response
                    .ok()
                    .map(|resp| parse_window_skeleton_batch(&resp.text, batch_start, &batch));

                // Per-batch progress tick. Atomic counter is the only
                // way to give the UI monotonic progress when batches
                // complete out of order under buffered().
                let done_now = completed.fetch_add(1, Ordering::SeqCst) + 1;
                let chunks_done =
                    (done_now * batch_size).min(chunk_count_for_progress);
                on_progress(IngestProgress::BuildingSkeleton {
                    done: chunks_done,
                    total: chunk_count_for_progress,
                });
                let _ = store
                    .update_asset_state(
                        &asset_id,
                        &AssetState::BuildingSkeleton {
                            chunks_done,
                            chunks_total: chunk_count_for_progress,
                        },
                    )
                    .await;

                (batch_idx, parsed)
            }
        })
        .buffered(T2_BATCH_CONCURRENCY)
        .collect()
        .await;

    let _ = total_batches; // referenced for future progress assertions; kept silent

    // Merge results sequentially after the parallel stream completes.
    // Order by batch_idx so the resulting sections list is in document
    // order — some downstream code reads sections in order.
    let mut sorted_results = batch_results;
    sorted_results.sort_by_key(|(idx, _)| *idx);
    let mut sections = Vec::new();
    let mut entity_mentions: std::collections::HashMap<String, Vec<usize>> =
        std::collections::HashMap::new();
    let mut entity_kinds: std::collections::HashMap<String, EntityKind> =
        std::collections::HashMap::new();
    let mut structural_moments = Vec::new();
    for (_, parsed_opt) in sorted_results {
        let Some(parsed) = parsed_opt else { continue };
        for entry in parsed {
            for (name, kind) in &entry.entity_names_and_kinds {
                entity_mentions
                    .entry(name.clone())
                    .or_default()
                    .push(entry.chunk_index);
                entity_kinds
                    .entry(name.clone())
                    .or_insert_with(|| kind.clone());
            }
            if let Some(ref desc) = entry.moment_description {
                structural_moments.push(StructuralMoment {
                    chunk_index: entry.chunk_index,
                    description: desc.clone(),
                    salience: 0.8,
                });
            }
            sections.push(SectionAnnotation {
                chunk_index: entry.chunk_index,
                function: entry.function,
                key_entities: entry
                    .entity_names_and_kinds
                    .iter()
                    .map(|(n, _)| n.clone())
                    .collect(),
                establishes: String::new(),
            });
        }
    }

    // ── Build entity ranking ────────────────────────────────
    let total_sections = sections.len().max(1);
    let mut main_entities: Vec<RankedEntity> = entity_mentions
        .iter()
        .map(|(name, indices)| {
            let first = indices.iter().copied().min().unwrap_or(0);
            let last = indices.iter().copied().max().unwrap_or(0);
            let presence_rate = indices.len() as f32 / total_sections as f32;
            let kind = entity_kinds
                .get(name)
                .cloned()
                .unwrap_or(EntityKind::Concept);
            RankedEntity {
                name: name.clone(),
                kind,
                presence_rate,
                first_appearance: first,
                last_appearance: last,
            }
        })
        .collect();
    main_entities.sort_by(|a, b| b.presence_rate.partial_cmp(&a.presence_rate).unwrap());
    main_entities.truncate(30);

    // ── Build entity index ──────────────────────────────────
    let entity_index: std::collections::HashMap<String, EntityAppearances> = entity_mentions
        .into_iter()
        .filter(|(name, _)| main_entities.iter().any(|e| &e.name == name))
        .map(|(name, indices)| {
            // Char-bounded truncation. The prior `c.content[..len.min(200)]`
            // byte-sliced and panicked when byte 200 landed inside a
            // multi-byte char (curly quotes, em-dashes, ellipses — common
            // in literary text). Same fix shape as `short_snippet`: take
            // chars not bytes.
            let quote_samples: Vec<String> = indices
                .iter()
                .take(3)
                .filter_map(|&i| chunks.get(i))
                .map(|c| c.content.chars().take(200).collect::<String>())
                .collect();
            (
                name,
                EntityAppearances {
                    chunk_indices: indices,
                    quote_samples,
                },
            )
        })
        .collect();

    structural_moments.truncate(40);

    // ── Action atoms (atlas-light) ──────────────────────────
    // For each top-N entity, run a Fast-slot pass over the entity's
    // appearance chunks and extract verb-object pairs anchored to
    // chunk_index. Cap N at 6 so the per-document cost stays bounded.
    // Action atoms route around the embedding-similarity gap — the
    // model queries by entity name, the tool consults the atom index,
    // and the original chunk surfaces by structural lookup rather
    // than embedding similarity.
    let actions = extract_action_atoms(inference, chunks, &main_entities, &entity_index).await;

    // T2-phase skeleton is partial — `overview` and `segments` are
    // empty placeholders. T3 (`build_and_persist_raptor_atlas` called
    // from `ingest`) fills them in: `overview` from the RAPTOR root
    // summary, `segments` from `extract_segments` (TextTiling).
    // This split is what powers the tiered state machine — the asset
    // transitions to `MultiHopReady` after this function returns,
    // before T3 enrichment starts.
    Ok(DocumentSkeleton {
        sections,
        main_entities,
        entity_index,
        structural_moments,
        overview: String::new(),
        actions,
        segments: Vec::new(),
        built_at: chrono::Utc::now(),
    })
}

/// Two-pass segment extraction.
///
/// **Pass A — boundary detection.** For each pair of adjacent
/// chunks, ask the model whether there is a segment break between
/// them. Output is a single word (`BREAK` or `CONTINUE`) — minimal
/// decode cost, accuracy is the model's job. ~N-1 calls for N
/// chunks.
///
/// **Pass B — segment naming.** Derive segment ranges from the
/// boundary decisions, then fire one call per segment to produce
/// title + summary + function. Output is bounded JSON.
///
/// Both passes use Speed::Slow (Primary 35B). Fast slot would
/// likely do well at Pass A (binary decision on adjacent chunks)
/// but is currently unloaded; revisit if ingest latency becomes a
/// production-perf blocker.
///
/// The function is fault-tolerant: any call failure falls back to
/// `CONTINUE` (no break) or a default title, so a partial network
/// blip produces fewer, larger segments rather than failing the
/// whole ingest. Segments are an additive retrieval surface, not
/// a load-bearing index — degraded extraction degrades retrieval
/// gracefully.
async fn extract_segments(
    inference: &Arc<dyn InferenceProvider>,
    chunks: &[TextChunk],
    main_entities: &[RankedEntity],
    doc_type: DocumentTypeTag,
    stored_embeddings: Option<Vec<Vec<f32>>>,
) -> Vec<DocumentSegment> {
    if chunks.len() < 2 {
        return Vec::new();
    }

    // ── Pass A — boundary detection via TextTiling ─────────
    //
    // Replaces the original per-pair LLM Pass A (one Speed::Slow
    // call per adjacent chunk pair, N-1 sequential calls for N
    // chunks — ~17 min on the Conrad 1006-chunk doc). TextTiling
    // (Hearst 1997, embedding variant) computes adjacent-chunk
    // cosine similarity, smooths it, scores each gap by its
    // "depth" (how far it dips below the surrounding peaks), and
    // thresholds at mean + k·std. Zero LLM calls; ~30s for
    // embedding + sub-second for the boundary detection.
    //
    // The earlier batched-LLM Pass A failed validation 2026-05-21
    // (template-shaped output, 5% precision). TextTiling has none
    // of that failure mode — boundaries fall out of arithmetic on
    // numbers the embedding model already produced for the chunk
    // store. The per-document-type cue is gone because the
    // similarity signal is doc-type-agnostic; doc-type-aware
    // naming still happens in Pass B.
    // Reuse T1's stored embeddings when the caller has them (they are
    // the SAME model + same chunk texts — re-embedding was pure waste,
    // ~30s per 300-chunk document, caught by the 2026-07-24 turbocharge
    // profile). Fall back to a fresh embed_batch when absent.
    let embeddings = match stored_embeddings {
        Some(e) if e.len() == chunks.len() => e,
        _ => {
            let texts: Vec<String> = chunks.iter().map(|c| c.content.clone()).collect();
            match inference.embed_batch(&texts).await {
                Ok(e) if e.len() == chunks.len() => e,
                _ => {
                    // Embedding failure or count mismatch — fall back to one
                    // segment per chunk so the rest of the pipeline still
                    // makes progress.
                    tracing::warn!(
                        "extract_segments — embed_batch failed or returned wrong count; treating doc as one segment per chunk"
                    );
                    vec![]
                }
            }
        }
    };
    let breaks: Vec<bool> = if embeddings.is_empty() {
        vec![false; chunks.len().saturating_sub(1)]
    } else {
        detect_segment_boundaries(&embeddings, /* window = */ 3, /* depth_k = */ 1.0)
    };
    tracing::info!(
        chunks = chunks.len(),
        breaks = breaks.iter().filter(|b| **b).count(),
        "extract_segments — TextTiling complete"
    );

    // Derive segment ranges from break decisions.
    let mut ranges: Vec<(usize, usize)> = Vec::new();
    let mut seg_start = 0usize;
    for (i, &is_break) in breaks.iter().enumerate() {
        if is_break {
            // Segment [seg_start..=i] ends. Next segment starts at i+1.
            ranges.push((seg_start, i));
            seg_start = i + 1;
        }
    }
    // Close the final segment, which runs to the last chunk.
    ranges.push((seg_start, chunks.len() - 1));

    // Cap segment count so a very-low depth_k or a pathological
    // embedding signal that fires breaks on every gap can't blow
    // up Pass B with hundreds of single-chunk segments.
    if ranges.len() > 200 {
        return Vec::new();
    }

    // ── Pass B — name ALL segments in one batched call ─────
    //
    // (2026-07-24 turbocharge arc.) The prior per-segment loop made
    // ~25-30 sequential ExtractDurable calls whose decode (~3k
    // tokens of title+summary+key_entities JSON) was the measured
    // wall of the T3 "silent block" (135s of a 285s subset build).
    // The briefing's scene map consumes ONLY `title` (+ chunk
    // range), so the batched schema emits `index|title|function`
    // lines — one call, ~15 tokens per segment, grammar-enforced
    // line count so alignment can't desync. `summary`/`key_entities`
    // are left empty (never read on the retrieval path; segments
    // carry structure, not content).
    let entity_list = main_entities
        .iter()
        .take(8)
        .map(|e| e.name.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    // Chunk the naming into calls of ≤14 segments, dispatched
    // concurrently.
    //
    // Two separate lessons are baked into this number.
    //
    // The 64 ceiling came from a correctness bug: the first cut of this
    // pass named all segments in ONE call clamped at 2048 output tokens
    // and silently placeholder-titled everything past segment ~85 on a
    // full book (caught by the 2026-07-24 quality gate).
    //
    // The drop from 64 to 14 came from the same arc's per-call ledger.
    // Naming is the only *decode*-bound call in the pipeline: 52
    // segments in one call meant 1288 completion tokens and 32.6s.
    // Split into four, the calls dispatch together and retire in
    // lockstep (three of them to the same millisecond) — measured
    // evidence that this path gets a batched decode — and the block
    // fell to 21.5s. Note the split does NOT bring the call inside the
    // FastShort claim: `ExtractDurable` is `LatencyClass::Normal`, so
    // gate 2 of `pick_slot` (`preferred_speed == Fast`) excludes it
    // regardless of size. Whatever coalesces these, it isn't that.
    //
    // What this costs on a host that serves these serially: each call
    // repeats the ~500-char instruction preamble, so total prompt grows
    // ~12% (3660 → 4088 tokens on the bench subset) for the same total
    // decode. That is the honest downside on a CPU-only box where
    // nothing batches. It is worth paying anyway because smaller calls
    // are also the fix for the truncation bug above — a short
    // grammar-forced output can't run out of budget mid-document.
    //
    // Order is preserved: `buffered` yields in input order.
    const PASS_B_CALL_SEGMENTS: usize = 14;
    // Matches the FastShort lane's `n_seq_max=8`; more in flight than
    // that cannot join a batch anywhere in the stack.
    const PASS_B_CONCURRENCY: usize = 8;
    let windows: Vec<(usize, Vec<(usize, usize)>)> = ranges
        .chunks(PASS_B_CALL_SEGMENTS)
        .enumerate()
        .map(|(w, window)| (w * PASS_B_CALL_SEGMENTS, window.to_vec()))
        .collect();
    let titles: Vec<(String, SectionFunction)> = stream::iter(windows)
        .map(|(base, window)| {
            let entity_list = entity_list.clone();
            let doc_type = doc_type.clone();
            async move {
                let n = window.len();
                let mut catalog = String::new();
                for (i, (start, end)) in window.iter().enumerate() {
                    let opening: String = chunks
                        .get(*start)
                        .map(|c| {
                            c.content
                                .chars()
                                .take(220)
                                .collect::<String>()
                                .replace('\n', " ")
                        })
                        .unwrap_or_default();
                    catalog
                        .push_str(&format!("#{} [chunks {start}..={end}] {opening}\n", base + i));
                }
                let prompt = format!(
                    "You are naming {n} segments of a {doc_type} document. Main document \
                     entities: {entity_list}. For EACH segment below write one line: \
                     <index>|<short title in the document's own register>|<function>, where \
                     function is one of Introduces, Develops, Complicates, Resolves, \
                     Transitions, Evidences. Output EXACTLY {n} lines in order, nothing else.\n\n\
                     Segments (index, chunk range, opening snippet):\n{catalog}\nAnswer ({n} lines):",
                    doc_type = doc_type.label(),
                );
                let mut start_rhs = String::from("line");
                for _ in 1..n {
                    start_rhs.push_str(" \"\\n\" line");
                }
                let lark_grammar = format!(
                    "start: {start_rhs}\n\
                     line: /[0-9]+/ \"|\" /[^|\\n]{{1,80}}/ \"|\" func\n\
                     func: \"Introduces\"|\"Develops\"|\"Complicates\"|\"Resolves\"|\"Transitions\"|\"Evidences\"\n",
                );
                // SLOT_POLICY §3 ExtractDurable: segment naming written to the
                // durable skeleton.
                let mut request = Workload::ExtractDurable.request(prompt)
                    .with_output_budget((((n * 24) + 40) as u32).min(2048));
                request.temperature = Some(0.1);
                request.lark_grammar = Some(lark_grammar);
                // POLICY-DEBT(SLOT_POLICY §3 ExtractDurable): Some(0) preserved for
                // P1 neutrality (bundle is None); P5 confirms.
                request.think_budget = Some(0);
                let mut call_titles: Vec<(String, SectionFunction)> =
                    match inference.complete(&request).await {
                        Ok(r) => parse_segment_title_lines(&r.text),
                        Err(_) => Vec::new(),
                    };
                call_titles.truncate(n);
                // Pad with placeholders so downstream position-matching stays
                // aligned even if a call under-delivered.
                while call_titles.len() < n {
                    call_titles.push((String::new(), SectionFunction::Develops));
                }
                call_titles
            }
        })
        .buffered(PASS_B_CONCURRENCY)
        .collect::<Vec<_>>()
        .await
        .into_iter()
        .flatten()
        .collect();
    let mut segments = Vec::new();
    for (i, (start, end)) in ranges.into_iter().enumerate() {
        let (title, function) = match titles.get(i) {
            Some((t, f)) if !t.is_empty() => (t.clone(), f.clone()),
            _ => (
                format!("Segment chunks {start}..={end}"),
                SectionFunction::Develops,
            ),
        };
        segments.push(DocumentSegment {
            id: format!("seg-{start}"),
            chunk_start: start,
            chunk_end: end,
            title,
            summary: String::new(),
            key_entities: Vec::new(),
            function,
        });
    }

    segments
}

/// Parse the batched Pass-B `index|title|function` lines. Position in
/// the output is authoritative (the grammar forces one line per
/// segment, in order); the leading index is advisory and ignored.
fn parse_segment_title_lines(text: &str) -> Vec<(String, SectionFunction)> {
    text.trim()
        .lines()
        .filter_map(|line| {
            let mut parts = line.splitn(3, '|');
            let _idx = parts.next()?;
            let title = parts.next()?.trim();
            if title.is_empty() {
                return None;
            }
            let function = match parts.next().map(str::trim).unwrap_or("Develops") {
                "Introduces" => SectionFunction::Introduces,
                "Complicates" => SectionFunction::Complicates,
                "Resolves" => SectionFunction::Resolves,
                "Transitions" => SectionFunction::Transitions,
                "Evidences" => SectionFunction::Evidences,
                _ => SectionFunction::Develops,
            };
            Some((title.to_string(), function))
        })
        .collect()
}

/// Build the RAPTOR atlas + motif index for an asset and persist them.
/// Runs inside the T3 phase of the tiered ingest pipeline. By this
/// point chunks-with-embeddings are guaranteed to be in the store
/// (T1 persisted them; T2 ran on them).
///
/// Emits `IngestProgress::BuildingSkeleton` progress events at coarse
/// phase boundaries (chunks-fetched, RAPTOR tree built, RAPTOR
/// persisted, motifs done) so the UI's progress bar moves through
/// the ~5-min T3 window. The progress fractions are mapped onto
/// `chunks_total` so the existing UI math (chunks_done / chunks_total)
/// continues to work — without this the bar would stay at 0/N for
/// the entire T3 duration, which made the May-22 fresh-ingest probe
/// look stuck on MultiHopReady.
///
/// Errors are logged and swallowed: the T2 skeleton is the durable
/// retrieval surface, RAPTOR is additive. A RAPTOR build failure
/// degrades briefing quality at Ready but never breaks attach.
/// Pure corpus-free RAPTOR + motif builder. Takes pre-fetched chunks
/// + embeddings and returns the artifacts the persistent variants
/// (attached-doc `build_and_persist_raptor_atlas`, folder
/// `FolderTieredProvider`) write into their respective tables.
///
/// Returns `Ok((nodes, motifs))` on success. `Err` is reserved for
/// RAPTOR-tree-build failures — motif extraction + classification is
/// best-effort (returns empty motif vec on classifier failure rather
/// than failing the whole call) because the briefing layer renders
/// motifs as additive: a missing motif index degrades signposts but
/// doesn't break retrieval.
///
/// `chunks` and `embeddings` MUST be the same length and in matching
/// order; the caller is responsible for filtering out chunks with
/// no embedding.
pub(crate) async fn build_atlas_artifacts(
    inference: &Arc<dyn InferenceProvider>,
    chunks: &[ChunkInput],
    embeddings: &[Vec<f32>],
    doc_type: DocumentTypeTag,
) -> Result<(Vec<RaptorNode>, Vec<AssetMotif>)> {
    build_atlas_artifacts_with_checkpoint(
        inference,
        chunks,
        embeddings,
        doc_type,
        None,
        None,
        None,
        crate::raptor_atlas::SummaryMode::Abstractive,
        None,
    )
    .await
}

/// RAPTOR tree only — no motif pass.
///
/// This is the entry point for the **folder/vault** path, and it is a
/// separate function rather than a flag on purpose: that path's motif
/// table (`conv_motifs`) had one INSERT, two DELETEs and no reader
/// anywhere in the workspace, while the pass itself cost **42.8% of a
/// cold vault build** (22.3m of 52m03s, 330 notes, measured
/// 2026-08-02). Deleting the write is only half the fix — as long as a
/// caller *could* ask this builder for motifs, the expensive pass can
/// come back by accident. It can't: the folder path calls a function
/// that has no motif concept in its return type.
///
/// The attached-document path keeps motifs and calls
/// [`build_atlas_artifacts_with_checkpoint`] instead — `asset_motifs`
/// is a different table with a real reader (`list_asset_motifs`) that
/// the document briefing renders.
pub(crate) async fn build_raptor_nodes_with_checkpoint(
    inference: &Arc<dyn InferenceProvider>,
    chunks: &[ChunkInput],
    embeddings: &[Vec<f32>],
    doc_type: DocumentTypeTag,
    checkpoint: Option<&crate::raptor_checkpoint::RaptorCheckpointHandle>,
    progress: Option<&Arc<dyn corpus_engine::enrichment::state::EnrichmentProgressSink>>,
    // User-authored summary correction, threaded to the RAPTOR
    // summarization prompt (the "flag a wrong summary" revision loop).
    correction_hint: Option<&str>,
    summary_mode: crate::raptor_atlas::SummaryMode,
    // T1 P1.2 override: `None` = the default gate (verify every
    // abstractive summary). Corpus-scale callers pass `Sample(p)` for
    // SP3 economics, or `Off` to opt out explicitly.
    verify_policy: Option<crate::summary_verify::VerifyPolicy>,
) -> Result<Vec<RaptorNode>> {
    if chunks.is_empty() {
        return Ok(Vec::new());
    }

    // RAPTOR tree — the long sub-phase. Errors propagate so callers
    // can transition state to Failed.
    //
    // Attached-document abstractive summaries are VERIFIER-GATED
    // (T1 P1.2): every LLM summary is decomposed into claims and
    // judged against its own member texts before persisting — pass,
    // one steered retry, or the extractive floor. Policy `On` because
    // per-document trees are small (tens of nodes); the SP3 sampling
    // economics apply to corpus-scale builds, which go through
    // `enrich raptor --verify-summaries` instead. Extractive builds
    // skip the gate by construction (quotes need no verification).
    let policy = verify_policy.unwrap_or(crate::summary_verify::VerifyPolicy::On);
    let verify = match (summary_mode, policy) {
        (crate::raptor_atlas::SummaryMode::Extractive, _)
        | (_, crate::summary_verify::VerifyPolicy::Off) => None,
        (crate::raptor_atlas::SummaryMode::Abstractive, policy) => {
            Some(Arc::new(crate::summary_verify::VerifyCtx {
                verifier: Arc::new(crate::summary_verify::JudgeSummaryVerifier::new(
                    Arc::clone(inference),
                )),
                policy,
                stats: Arc::new(crate::summary_verify::VerifyStats::default()),
            }))
        }
    };
    let t_tree = std::time::Instant::now();
    let nodes = crate::raptor_atlas::build_raptor_atlas_with_verify(
        inference,
        chunks,
        embeddings,
        doc_type.clone(),
        checkpoint,
        progress,
        correction_hint,
        summary_mode,
        verify.clone(),
    )
    .await
    .map_err(|e| Error::Execution(format!("build_raptor_atlas: {e}")))?;
    let tree_s = t_tree.elapsed().as_secs_f32();
    if let Some(ctx) = verify.as_ref() {
        tracing::info!(
            stats = %ctx.stats.summary_line(),
            "document_asset: summary verification gate (T1 P1.2)"
        );
    }

    // [t3-profile] turbocharge-arc phase split (2026-07-24) — stderr on
    // the driving process; promote to allowlisted tracing spans when the
    // arc lands.
    eprintln!(
        "      [t3-profile] raptor_tree={tree_s:.1}s (nodes={})",
        nodes.len()
    );

    Ok(nodes)
}

/// RAPTOR tree **plus** the TF-IDF motif index — the attached-document
/// path, whose `asset_motifs` rows the document briefing actually
/// renders.
///
/// See [`build_raptor_nodes_with_checkpoint`] for why the folder/vault
/// path deliberately cannot reach this function.
pub(crate) async fn build_atlas_artifacts_with_checkpoint(
    inference: &Arc<dyn InferenceProvider>,
    chunks: &[ChunkInput],
    embeddings: &[Vec<f32>],
    doc_type: DocumentTypeTag,
    checkpoint: Option<&crate::raptor_checkpoint::RaptorCheckpointHandle>,
    progress: Option<&Arc<dyn corpus_engine::enrichment::state::EnrichmentProgressSink>>,
    correction_hint: Option<&str>,
    summary_mode: crate::raptor_atlas::SummaryMode,
    verify_policy: Option<crate::summary_verify::VerifyPolicy>,
) -> Result<(Vec<RaptorNode>, Vec<AssetMotif>)> {
    if chunks.is_empty() {
        return Ok((Vec::new(), Vec::new()));
    }

    let nodes = build_raptor_nodes_with_checkpoint(
        inference,
        chunks,
        embeddings,
        doc_type.clone(),
        checkpoint,
        progress,
        correction_hint,
        summary_mode,
        verify_policy,
    )
    .await?;

    // Convert ChunkInput → TextChunk for the existing motif extractor.
    let t_motifs = std::time::Instant::now();
    let text_chunks: Vec<TextChunk> = chunks
        .iter()
        .map(|c| TextChunk {
            content: c.content.clone(),
            index: c.chunk_id as usize,
        })
        .collect();
    // Wider candidate pool (was 100) since the df>=1 floor lets
    // rare-but-distinctive scene markers reach the LLM classifier.
    let candidates = extract_motif_candidates(&text_chunks, 200);
    let motifs = classify_motifs(inference, candidates, doc_type).await;
    // `motifs→` is the classified count; the old label said
    // `motif_candidates→` and was reading the wrong side of
    // `classify_motifs`.
    eprintln!(
        "      [t3-profile] motifs={:.1}s (motifs→{})",
        t_motifs.elapsed().as_secs_f32(),
        motifs.len(),
    );

    Ok((nodes, motifs))
}

async fn build_and_persist_raptor_atlas(
    inference: &Arc<dyn InferenceProvider>,
    store: &Arc<dyn StateStore>,
    asset_id: &str,
    source_key: &str,
    doc_type: DocumentTypeTag,
    on_progress: &Arc<dyn Fn(IngestProgress) + Send + Sync>,
    chunks_total: usize,
) {
    let started = std::time::Instant::now();
    tracing::info!(asset_id, "raptor_atlas: starting T3 build");

    // Helper to emit + persist a coarse progress checkpoint. The
    // fractions are deliberate guesses — RAPTOR's leaf-summarisation
    // doesn't expose per-cluster progress, so we mark phase
    // boundaries instead. UI shows monotonic movement; users see
    // "something is happening" instead of a frozen 0/N bar.
    let emit = |fraction: f32| {
        let done = ((chunks_total as f32 * fraction).round() as usize).min(chunks_total);
        on_progress(IngestProgress::BuildingSkeleton {
            done,
            total: chunks_total,
        });
        let asset_id = asset_id.to_string();
        let store = Arc::clone(store);
        // Fire-and-forget the state update — failure is non-fatal,
        // the UI just doesn't show this checkpoint. Spawn so we
        // don't block the T3 build path on the write.
        tokio::spawn(async move {
            let _ = store
                .update_asset_state(
                    &asset_id,
                    &AssetState::BuildingSkeleton {
                        chunks_done: done,
                        chunks_total,
                    },
                )
                .await;
        });
    };

    // Fetch chunks (which carry embeddings from the embed phase).
    let chunks = match store.get_chunks_by_source(source_key).await {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(asset_id, error = %e, "raptor_atlas: get_chunks_by_source failed");
            return;
        }
    };
    emit(0.20);
    let total = chunks.len();
    let mut raptor_chunks: Vec<ChunkInput> = Vec::with_capacity(total);
    let mut embeddings: Vec<Vec<f32>> = Vec::with_capacity(total);
    for c in &chunks {
        if let Some(emb) = c.embedding.as_ref() {
            raptor_chunks.push(ChunkInput {
                chunk_id: c.chunk_index as u32,
                content: c.content.clone(),
            });
            embeddings.push(emb.clone());
        }
    }
    if raptor_chunks.is_empty() {
        tracing::warn!(
            asset_id,
            total,
            "raptor_atlas: no embedded chunks; skipping"
        );
        return;
    }

    // Build artifacts via the corpus-free helper. Errors here are
    // RAPTOR-tree-build failures (the only Err path); motif extraction
    // is best-effort inside the helper and returns an empty vec on
    // classifier failure.
    let (nodes, motifs) =
        match build_atlas_artifacts(inference, &raptor_chunks, &embeddings, doc_type).await {
            Ok(pair) => pair,
            Err(e) => {
                tracing::warn!(asset_id, error = %e, "raptor_atlas: build_atlas_artifacts failed");
                return;
            }
        };
    let node_count = nodes.len();
    let motif_count = motifs.len();
    let distinctive_count = motifs.iter().filter(|m| m.is_distinctive).count();
    // RAPTOR + motif build complete — mark ~75% so the bar moved
    // through the longest opaque wait.
    emit(0.75);

    if let Err(e) = store.save_raptor_nodes(asset_id, &nodes).await {
        tracing::warn!(asset_id, error = %e, "raptor_atlas: save_raptor_nodes failed");
        return;
    }
    emit(0.80);

    if let Err(e) = store.save_asset_motifs(asset_id, &motifs).await {
        tracing::warn!(asset_id, error = %e, "raptor_atlas: save_asset_motifs failed");
        return;
    }
    emit(0.95);

    tracing::info!(
        asset_id,
        chunks = raptor_chunks.len(),
        nodes = node_count,
        motif_candidates = motif_count,
        distinctive_motifs = distinctive_count,
        elapsed_ms = started.elapsed().as_millis() as u64,
        "raptor_atlas: T3 build complete"
    );
}

/// Stoplist of the most common English function words. Used by motif
/// extraction to filter out conjunctions, prepositions, pronouns, and
/// other words that recur frequently in every English document and
/// therefore can't distinguish one document from another. Kept short
/// and curated (~110 entries) rather than exhaustive — the TF-IDF +
/// LLM classifier downstream catches anything this misses.
const MOTIF_STOPLIST: &[&str] = &[
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
fn extract_motif_candidates(chunks: &[TextChunk], top_n: usize) -> Vec<MotifCandidate> {
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
struct MotifCandidate {
    term: String,
    tf_idf_score: f32,
    occurrence_chunk_ids: Vec<u32>,
}

/// Ask the model which of the candidate terms are genuine recurring
/// motifs vs incidental rare words. One Slow-slot call; grammar
/// forces a JSON array of motif terms drawn from the input set.
///
/// Returns a Vec<AssetMotif> with `is_distinctive` set per the
/// model's judgment. Falls back to "all distinctive" on LLM failure
/// — over-inclusive is safer than empty (the briefing has its own
/// budget cap).
async fn classify_motifs(
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
fn parse_motif_classification(text: &str) -> std::collections::HashSet<String> {
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
fn detect_segment_boundaries(embeddings: &[Vec<f32>], window: usize, depth_k: f32) -> Vec<bool> {
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
fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
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
fn parse_lean_skeleton_batch(
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
fn parse_window_skeleton_batch(
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
fn parse_entity_name_list(response: &str) -> Vec<String> {
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
fn attribute_entity_names(
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
fn parse_segment_naming(text: &str) -> Option<(String, String, SectionFunction, Vec<String>)> {
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

/// Run a Fast-slot extraction over the top-N entities' chunks and
/// emit `ActionAtom`s. One LLM call per entity, batching that entity's
/// appearance chunks into a single prompt. The model is asked for a
/// JSON list of `{verb, object, chunk_index, evidence}`; any chunk
/// we can't parse cleanly is silently dropped — atoms are an additive
/// retrieval surface, not a load-bearing index, so missing data is
/// degraded behaviour, not a failure mode.
async fn extract_action_atoms(
    inference: &Arc<dyn InferenceProvider>,
    chunks: &[TextChunk],
    main_entities: &[RankedEntity],
    entity_index: &std::collections::HashMap<String, EntityAppearances>,
) -> Vec<ActionAtom> {
    // ── Build every entity's prompt first, then fan out.
    //
    // These six calls used to run in a sequential `for` loop: ~3s each,
    // ~19s of the T2 tail, all of it on the critical path between
    // rag_available and multi_hop_ready. They're independent — each
    // entity's atoms depend only on that entity's sampled chunks — so
    // the serialization bought nothing. Prompt construction stays here
    // (it borrows `chunks`/`entity_index`); only the calls fan out.
    struct AtomCall {
        entity: String,
        sample_indices: Vec<usize>,
        prompt: String,
    }
    let mut calls: Vec<AtomCall> = Vec::new();
    // Top-6: covers the load-bearing characters/concepts in a typical
    // narrative; running the full top-30 would blow the budget for
    // marginal lift on peripheral entities.
    for ent in main_entities.iter().take(6) {
        let Some(appearances) = entity_index.get(&ent.name) else {
            continue;
        };
        // Cap appearance chunks at 6 to bound the prompt size. The
        // entity's earliest appearances are usually introductory;
        // we sample stride-wise from the appearance list so we cover
        // beginning, middle, end of the entity's arc.
        let total = appearances.chunk_indices.len();
        let sample_indices: Vec<usize> = if total <= 6 {
            appearances.chunk_indices.clone()
        } else {
            let stride = (total / 6).max(1);
            appearances
                .chunk_indices
                .iter()
                .step_by(stride)
                .take(6)
                .copied()
                .collect()
        };

        // Compose a single prompt listing the sampled chunks with
        // their indices. Cap each chunk excerpt at 500 chars so the
        // total prompt stays inside Fast-slot context limits even
        // when an entity hits 6 long chunks.
        let mut passages = String::new();
        for &idx in &sample_indices {
            if let Some(chunk) = chunks.get(idx) {
                let excerpt: String = chunk.content.chars().take(500).collect();
                passages.push_str(&format!("\n[chunk {idx}]\n{}\n", excerpt.trim(),));
            }
        }
        if passages.trim().is_empty() {
            continue;
        }

        let prompt = format!(
            "Extract what \"{name}\" DOES in these passages. For each chunk \
             where {name} performs a notable action, emit one JSON object:\n\
             {{\"chunk_index\": <int>, \"verb\": \"<lowercase verb>\", \
             \"object\": \"<short noun phrase>\", \"evidence\": \"<verbatim snippet ≤140 chars>\"}}\n\n\
             Rules:\n\
             - Skip chunks where {name} is only mentioned in passing.\n\
             - Verb is a single lowercase past-tense verb (e.g. \"stitched\", \"discovered\", \"killed\").\n\
             - Object is what the verb acts on, in the document's own wording.\n\
             - Evidence is verbatim text from the chunk, ≤140 chars, that contains the verb+object.\n\
             - Skip if nothing notable happens to/by {name} in the chunk.\n\n\
             Passages:\n{passages}\n\n\
             Respond with a JSON array, no commentary:\n[",
            name = ent.name,
        );

        calls.push(AtomCall {
            entity: ent.name.clone(),
            sample_indices,
            prompt,
        });
    }

    // Fan out. `buffered` yields in input order, so the atom list stays
    // in main-entity rank order exactly as the sequential loop left it.
    const ATOM_CONCURRENCY: usize = 6;
    let responses: Vec<(AtomCall, Option<String>)> = stream::iter(calls)
        .map(|call| {
            let inference = Arc::clone(inference);
            async move {
                // SLOT_POLICY §3 Housekeep: per-entity action-atom extraction —
                // advisory enrichment kept on the Fast slot (P1 neutrality).
                // Housekeep's Some(0) think budget matches this site verbatim.
                let mut request =
                    Workload::Housekeep.request(call.prompt.clone())
                        // POLICY-DEBT(SLOT_POLICY §4.5 Housekeep): 768 > 512 forfeits the
                        // batched FastShort claim; the JSON action array needs the room.
                        .with_output_budget(768);
                request.temperature = Some(0.1);
                let text = match inference.complete(&request).await {
                    Ok(r) => Some(r.text),
                    Err(e) => {
                        tracing::debug!(entity = %call.entity, error = %e, "extract_action_atoms — LLM call failed; skipping entity");
                        None
                    }
                };
                (call, text)
            }
        })
        .buffered(ATOM_CONCURRENCY)
        .collect()
        .await;

    let mut out: Vec<ActionAtom> = Vec::new();
    for (call, text) in responses {
        let Some(text) = text else { continue };
        let ent_name = call.entity;
        let sample_indices = call.sample_indices;

        // Tolerant JSON parse — the model sometimes wraps the array
        // in ```json fences or appends explanatory prose. Isolate
        // the first `[` to the last `]`.
        let start = text.find('[');
        let end = text.rfind(']');
        let (start, end) = match (start, end) {
            (Some(s), Some(e)) if e > s => (s, e),
            _ => continue,
        };
        let payload = &text[start..=end];
        let parsed: Vec<ActionAtomDraft> = match serde_json::from_str(payload) {
            Ok(p) => p,
            Err(e) => {
                tracing::debug!(
                    entity = %ent_name,
                    error = %e,
                    payload = %payload.chars().take(200).collect::<String>(),
                    "extract_action_atoms — parse failed; skipping entity"
                );
                continue;
            }
        };

        for draft in parsed {
            // Sanity: drop atoms whose chunk_index isn't in the
            // sampled set — the model occasionally hallucinates
            // chunk numbers when extracting.
            if !sample_indices.contains(&draft.chunk_index) {
                continue;
            }
            let evidence = draft.evidence.trim();
            if evidence.is_empty() {
                continue;
            }
            out.push(ActionAtom {
                entity: ent_name.clone(),
                verb: draft.verb.trim().to_lowercase(),
                object: draft.object.trim().to_string(),
                chunk_index: draft.chunk_index,
                evidence: evidence.chars().take(140).collect(),
            });
        }
    }

    tracing::info!(atoms = out.len(), "extract_action_atoms — done");
    out
}

#[derive(Debug, Deserialize)]
struct ActionAtomDraft {
    chunk_index: usize,
    verb: String,
    object: String,
    evidence: String,
}

/// Generate a one-paragraph overview of the document.
async fn generate_overview(
    inference: &Arc<dyn InferenceProvider>,
    chunks: &[TextChunk],
    doc_type: &DocumentTypeTag,
) -> String {
    let sample: String = chunks
        .iter()
        .take(5)
        .map(|c| c.content.as_str())
        .collect::<Vec<_>>()
        .join("\n\n");

    let prompt = format!(
        "Write a single paragraph (3-5 sentences) overview of this {doc_type} document \
         based on its opening sections. Focus on: what it's about, the main entities \
         or characters, and the central question or theme.\n\n\
         Opening:\n{sample}\n\n\
         Overview:",
        doc_type = doc_type.label(),
    );

    // SLOT_POLICY §3 Housekeep: one-paragraph document overview —
    // advisory context, not durable truth. Housekeep's Some(0) think
    // budget matches this site verbatim.
    let mut request = Workload::Housekeep.request(prompt).with_output_budget(256);
    request.temperature = Some(0.3);
    inference
        .complete(&request)
        .await
        .map(|r| r.text)
        .unwrap_or_else(|_| "Overview not available.".to_string())
}

// ─── Parsing helpers ─────────────────────────────────────────

/// Parsed result from one batch of skeleton extraction.
struct SkeletonBatchEntry {
    chunk_index: usize,
    function: SectionFunction,
    /// Entity names paired with their classified kind.
    entity_names_and_kinds: Vec<(String, EntityKind)>,
    moment_description: Option<String>,
}

/// Parse an entity kind string from the LLM response.
fn parse_entity_kind(s: &str) -> EntityKind {
    match s.to_lowercase().as_str() {
        "character" => EntityKind::Character,
        "argument" => EntityKind::Argument,
        "concept" => EntityKind::Concept,
        "claim" => EntityKind::Claim,
        "evidence" => EntityKind::Evidence,
        "theme" => EntityKind::Theme,
        "person" => EntityKind::Person,
        "event" => EntityKind::Event,
        _ => EntityKind::Concept,
    }
}

/// Parse the LLM's JSON response for a skeleton extraction batch.
/// Tolerant of malformed JSON — returns None for unparseable responses
/// rather than failing the whole pipeline.
///
/// Handles two entity formats:
/// - `"key_entities": [{"name": "X", "kind": "Character"}]` (preferred)
/// - `"key_entities": ["X", "Y"]` (fallback — kind defaults to Concept)
fn parse_skeleton_batch(response: &str, batch_start: usize) -> Option<Vec<SkeletonBatchEntry>> {
    let trimmed = response.trim();
    let json_start = trimmed.find('[')?;
    let json_end = trimmed.rfind(']')?;
    if json_end <= json_start {
        return None;
    }
    let json_str = &trimmed[json_start..=json_end];

    let arr: Vec<serde_json::Value> = serde_json::from_str(json_str).ok()?;

    let mut entries = Vec::new();
    for (i, obj) in arr.iter().enumerate() {
        let function_str = obj.get("function")?.as_str()?;
        let function = match function_str.to_lowercase().as_str() {
            "introduces" => SectionFunction::Introduces,
            "develops" => SectionFunction::Develops,
            "complicates" => SectionFunction::Complicates,
            "resolves" => SectionFunction::Resolves,
            "transitions" => SectionFunction::Transitions,
            "evidences" => SectionFunction::Evidences,
            _ => SectionFunction::Develops,
        };

        // Parse entities — handle both object and string formats.
        let entity_names_and_kinds: Vec<(String, EntityKind)> = obj
            .get("key_entities")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| {
                        if let Some(obj) = v.as_object() {
                            // {"name": "X", "kind": "Character"}
                            let name = obj.get("name")?.as_str()?.to_string();
                            let kind = obj
                                .get("kind")
                                .and_then(|k| k.as_str())
                                .map(parse_entity_kind)
                                .unwrap_or(EntityKind::Concept);
                            Some((name, kind))
                        } else {
                            v.as_str().map(|s| (s.to_string(), EntityKind::Concept))
                        }
                    })
                    .collect()
            })
            .unwrap_or_default();

        let is_moment = obj
            .get("is_structural_moment")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        let moment_description = if is_moment {
            obj.get("moment_description")
                .and_then(|v| v.as_str())
                .map(String::from)
        } else {
            None
        };

        entries.push(SkeletonBatchEntry {
            chunk_index: batch_start + i,
            function,
            entity_names_and_kinds,
            moment_description,
        });
    }

    Some(entries)
}

/// Parse the router's JSON response into a DocumentAssetOperation.
/// Falls back to RAG if the response is unparseable.
fn parse_route_response(response: &str, original_request: &str) -> Result<DocumentAssetOperation> {
    let trimmed = response.trim();
    // Find JSON object in the response.
    let json_start = trimmed.find('{');
    let json_end = trimmed.rfind('}');

    if let (Some(start), Some(end)) = (json_start, json_end) {
        if end > start {
            if let Ok(obj) = serde_json::from_str::<serde_json::Value>(&trimmed[start..=end]) {
                let op = obj.get("op").and_then(|v| v.as_str()).unwrap_or("rag");
                return Ok(match op {
                    "synthesis" => DocumentAssetOperation::Synthesis {
                        focus: obj
                            .get("focus")
                            .and_then(|v| v.as_str())
                            .unwrap_or(original_request)
                            .to_string(),
                        entities: obj
                            .get("entities")
                            .and_then(|v| v.as_array())
                            .map(|arr| {
                                arr.iter()
                                    .filter_map(|v| v.as_str().map(String::from))
                                    .collect()
                            })
                            .unwrap_or_default(),
                    },
                    "aggregation" => DocumentAssetOperation::Aggregation {
                        query: obj
                            .get("query")
                            .and_then(|v| v.as_str())
                            .unwrap_or(original_request)
                            .to_string(),
                    },
                    "transformation" => DocumentAssetOperation::Transformation,
                    "off_topic" => DocumentAssetOperation::OffTopic {
                        reason: obj
                            .get("reason")
                            .and_then(|v| v.as_str())
                            .unwrap_or("unrelated question")
                            .to_string(),
                    },
                    _ => DocumentAssetOperation::Rag {
                        query: obj
                            .get("query")
                            .and_then(|v| v.as_str())
                            .unwrap_or(original_request)
                            .to_string(),
                    },
                });
            }
        }
    }

    // Fallback: unparseable response → treat as RAG.
    Ok(DocumentAssetOperation::Rag {
        query: original_request.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The property that makes the GLiNER swap safe: the extractor
    /// contract returns LOWER-CASED names, but what we store must be
    /// cased the way the document casts it — these strings reach the
    /// briefing and the segment-naming prompt.
    #[test]
    fn attribution_recovers_document_casing_from_lowercased_names() {
        let batch = [
            chunk("Mr Verloc walked on. Stevie followed him."),
            chunk("Later, Chief Inspector Heat considered the case."),
        ];
        // Exactly what `EntityExtractor::extract_entities` promises.
        let names = vec![
            "mr verloc".to_string(),
            "stevie".to_string(),
            "chief inspector heat".to_string(),
        ];

        let entries = attribute_entity_names(names, 0, &batch);
        let first: Vec<&str> = entries[0]
            .entity_names_and_kinds
            .iter()
            .map(|(n, _)| n.as_str())
            .collect();
        assert!(first.contains(&"Mr Verloc"), "{first:?}");
        assert!(first.contains(&"Stevie"), "{first:?}");
        let second: Vec<&str> = entries[1]
            .entity_names_and_kinds
            .iter()
            .map(|(n, _)| n.as_str())
            .collect();
        assert_eq!(second, vec!["Chief Inspector Heat"], "{second:?}");
    }

    /// A name no chunk contains verbatim still lands somewhere rather
    /// than vanishing from the index.
    #[test]
    fn attribution_falls_back_to_first_chunk_for_unmatched_names() {
        let batch = [chunk("The shop was dim."), chunk("Rain fell.")];
        let entries = attribute_entity_names(vec!["the Professor".to_string()], 7, &batch);
        assert_eq!(entries[0].chunk_index, 7, "batch_start offset preserved");
        assert_eq!(entries[0].entity_names_and_kinds.len(), 1);
        // Unmatched keeps the caller's spelling — there's no document
        // occurrence to recover casing from.
        assert_eq!(entries[0].entity_names_and_kinds[0].0, "the Professor");
        assert!(entries[1].entity_names_and_kinds.is_empty());
    }

    /// The LLM path must keep behaving exactly as before the split.
    #[test]
    fn llm_window_parse_still_attributes_across_chunks() {
        let batch = [
            chunk("Winnie said nothing."),
            chunk("Winnie and Ossipon spoke."),
        ];
        let entries = parse_window_skeleton_batch("Winnie, Ossipon", 0, &batch);
        let names0: Vec<&str> = entries[0]
            .entity_names_and_kinds
            .iter()
            .map(|(n, _)| n.as_str())
            .collect();
        let names1: Vec<&str> = entries[1]
            .entity_names_and_kinds
            .iter()
            .map(|(n, _)| n.as_str())
            .collect();
        assert_eq!(names0, vec!["Winnie"]);
        assert_eq!(names1, vec!["Winnie", "Ossipon"]);
    }

    #[test]
    fn entity_name_list_tolerates_answer_prefix_and_blank_items() {
        let names = parse_entity_name_list("Answer: Stevie, , Mr Verloc,\n Winnie ");
        assert_eq!(names, vec!["Stevie", "Mr Verloc", "Winnie"]);
    }

    #[test]
    fn parse_route_response_parses_off_topic_with_reason() {
        let resp = r#"{"op": "off_topic", "reason": "different domain"}"#;
        match parse_route_response(resp, "user q").unwrap() {
            DocumentAssetOperation::OffTopic { reason } => {
                assert_eq!(reason, "different domain");
            }
            other => panic!("expected OffTopic, got {other:?}"),
        }
    }

    #[test]
    fn parse_route_response_off_topic_default_reason() {
        let resp = r#"{"op": "off_topic"}"#;
        match parse_route_response(resp, "user q").unwrap() {
            DocumentAssetOperation::OffTopic { reason } => {
                assert_eq!(reason, "unrelated question");
            }
            other => panic!("expected OffTopic, got {other:?}"),
        }
    }

    #[test]
    fn parse_route_response_unknown_op_falls_back_to_rag() {
        let resp = r#"{"op": "nonsense"}"#;
        match parse_route_response(resp, "user q").unwrap() {
            DocumentAssetOperation::Rag { query } => {
                assert_eq!(query, "user q");
            }
            other => panic!("expected Rag, got {other:?}"),
        }
    }

    // ── Self-reference detection ────────────────────────────────

    #[test]
    fn self_reference_matches_common_phrasings() {
        for q in &[
            "Can you summarize this document?",
            "What is this paper about?",
            "summarize the text",
            "What does this say about consciousness?",
            "give me a summary of this",
            "Tell me what the attached is about",
            "what's this about?",
            "Summarize the article please",
        ] {
            assert!(
                detect_self_reference(q),
                "expected self-reference match for {q:?}"
            );
        }
    }

    #[test]
    fn self_reference_rejects_off_topic_questions() {
        for q in &[
            "What is the difference between Theravada and Zen buddhism?",
            "Explain quantum superposition",
            "Who was Erwin Schrödinger?",
            "Give me a recipe for banana bread",
        ] {
            assert!(
                !detect_self_reference(q),
                "expected NO self-reference match for {q:?}"
            );
        }
    }

    #[test]
    fn self_reference_is_case_insensitive() {
        assert!(detect_self_reference("SUMMARIZE THIS DOCUMENT"));
        assert!(detect_self_reference("What Is This Paper About?"));
    }

    // ── Filename grounding ───────────────────────────────────────

    fn test_asset(title: &str, filename: &str) -> DocumentAsset {
        DocumentAsset {
            id: "t".to_string(),
            title: title.to_string(),
            filename: filename.to_string(),
            file_size_mb: 0.0,
            word_count: 0,
            chunk_count: 0,
            document_type: DocumentTypeTag::Unknown,
            ingested_at: chrono::Utc::now(),
            index_id: "t".to_string(),
            skeleton: None,
            state: AssetState::PartiallyReady,
            owner: None,
        }
    }

    #[test]
    fn filename_tokens_drops_stopwords_numbers_short_tokens() {
        let asset = test_asset(
            "11. Erwin Schrodinger   What is Life  1944",
            "11._Erwin_Schrodinger_-_What_is_Life__1944_.pdf",
        );
        let toks = filename_tokens(&asset);
        assert!(toks.contains(&"erwin".to_string()));
        assert!(toks.contains(&"schrodinger".to_string()));
        assert!(toks.contains(&"life".to_string()));
        // stopwords dropped
        assert!(!toks.iter().any(|t| t == "the" || t == "is"));
        // short / digit-only tokens dropped
        assert!(!toks.iter().any(|t| t == "11" || t == "1944"));
        // extensions stripped by the stopword list
        assert!(!toks.contains(&"pdf".to_string()));
    }

    #[test]
    fn mentions_document_matches_author_name() {
        let asset = test_asset(
            "Erwin Schrodinger - What is Life",
            "Erwin_Schrodinger_What_is_Life.pdf",
        );
        let toks = filename_tokens(&asset);
        assert!(mentions_document(
            &toks,
            "What does Schrödinger say about consciousness?"
        ));
        assert!(mentions_document(&toks, "Explain Erwin's thesis"));
    }

    #[test]
    fn mentions_document_rejects_unrelated_question() {
        let asset = test_asset(
            "Erwin Schrodinger - What is Life",
            "Erwin_Schrodinger_What_is_Life.pdf",
        );
        let toks = filename_tokens(&asset);
        assert!(!mentions_document(
            &toks,
            "What is the difference between Theravada and Zen buddhism?"
        ));
        assert!(!mentions_document(&toks, "Who won the 2018 World Cup?"));
    }

    #[test]
    fn mentions_document_unicode_folds_to_ascii() {
        let asset = test_asset("Erwin Schrödinger", "Erwin_Schrodinger.pdf");
        let toks = filename_tokens(&asset);
        // Token is folded when extracted — "schrödinger" → "schrodinger".
        assert!(toks.contains(&"schrodinger".to_string()));
        // Question with the accented form still matches because the query
        // is ASCII-folded at match time.
        assert!(mentions_document(&toks, "What did Schrödinger argue?"));
    }

    #[test]
    fn short_snippet_truncates_at_word_boundary() {
        let content = "The aperiodic crystal described by Schrödinger is a molecular \
                       structure found in chromosomes that differs radically from periodic \
                       crystals studied by physicists. It stores hereditary information.";
        let snip = short_snippet(content, 60);
        assert!(snip.ends_with("..."));
        assert!(!snip.contains("Schrödinger") || snip.len() <= 60 + "...".len() + 10 /* slack */);
        // Snippet ends on a word boundary — no mid-word cut before the ellipsis.
        let pre = snip.trim_end_matches("...");
        assert!(pre.ends_with(|c: char| c.is_ascii_alphanumeric() || c == ','));
    }

    #[test]
    fn short_snippet_returns_input_when_under_max() {
        assert_eq!(short_snippet("hello", 100), "hello");
    }

    #[test]
    fn short_snippet_handles_multibyte_at_boundary() {
        // Regression: Conrad's text uses U+201C / U+201D curly quotes
        // (3 bytes each in UTF-8). Passing `max` such that the byte
        // index lands inside one of those quotes used to panic with
        // "end byte index N is not a char boundary". This is the input
        // shape that book-report bench v1.1 caught on the Tier-4
        // winnie_incurious_motif question.
        let content = "Mr Verloc observed quietly\u{201C}I have no means of action upon the police here.\u{201D} Vladimir replied at length.";
        // Find the byte index of the opening curly quote and ask for a
        // snippet that lands inside it.
        let quote_byte = content
            .find('\u{201C}')
            .expect("test fixture must contain U+201C");
        let inside_quote = quote_byte + 1; // mid-character; would panic on raw slice
        let snip = short_snippet(content, inside_quote);
        // Must not panic AND must end with the ellipsis sentinel.
        assert!(
            snip.ends_with("..."),
            "expected snippet to end with `...`, got: {snip:?}"
        );
    }

    #[test]
    fn mentions_document_word_boundary_avoids_partial_matches() {
        // Token "life" must not match inside "wildlife".
        let asset = test_asset("What is Life", "What_is_Life.pdf");
        let toks = filename_tokens(&asset);
        assert!(toks.contains(&"life".to_string()));
        assert!(!mentions_document(
            &toks,
            "Tell me about the wildlife of Africa"
        ));
        // But "life" as a standalone word still matches.
        assert!(mentions_document(&toks, "Explain the meaning of life"));
    }

    #[test]
    fn parse_route_response_existing_variants_still_work() {
        // Guard against regression in the off_topic branch addition.
        let rag = parse_route_response(r#"{"op":"rag","query":"foo"}"#, "orig").unwrap();
        assert!(matches!(rag, DocumentAssetOperation::Rag { .. }));

        let syn = parse_route_response(
            r#"{"op":"synthesis","focus":"themes","entities":["A","B"]}"#,
            "orig",
        )
        .unwrap();
        match syn {
            DocumentAssetOperation::Synthesis { focus, entities } => {
                assert_eq!(focus, "themes");
                assert_eq!(entities, vec!["A".to_string(), "B".to_string()]);
            }
            other => panic!("expected Synthesis, got {other:?}"),
        }

        let agg = parse_route_response(r#"{"op":"aggregation","query":"x"}"#, "orig").unwrap();
        assert!(matches!(agg, DocumentAssetOperation::Aggregation { .. }));

        let xform = parse_route_response(r#"{"op":"transformation"}"#, "orig").unwrap();
        assert!(matches!(xform, DocumentAssetOperation::Transformation));
    }

    // ── TextTiling boundary detection ───────────────────────────

    /// Build a synthetic embedding sequence: `cluster_count` clusters
    /// of `per_cluster` chunks each, where within-cluster chunks share
    /// a near-identical embedding and between-cluster chunks are
    /// near-orthogonal. The expected break pattern is "false ×
    /// (per_cluster-1), true, false × (per_cluster-1), true, ..."
    fn synthetic_clusters(cluster_count: usize, per_cluster: usize) -> Vec<Vec<f32>> {
        let mut out = Vec::new();
        for c in 0..cluster_count {
            let mut base = vec![0.01; 8];
            base[c % 8] = 1.0;
            for k in 0..per_cluster {
                // Tiny within-cluster jitter so cosine isn't exactly 1.0.
                let mut v = base.clone();
                v[(c + k + 1) % 8] += 0.02;
                out.push(v);
            }
        }
        out
    }

    #[test]
    fn texttiling_fires_breaks_at_cluster_transitions() {
        // 3 clusters × 4 chunks = 12 chunks, 11 gaps. Expected:
        // breaks at gaps 3 and 7 (between cluster 0/1 and 1/2).
        let embeddings = synthetic_clusters(3, 4);
        let breaks = detect_segment_boundaries(&embeddings, 3, 0.5);
        assert_eq!(breaks.len(), 11);

        // The two cluster-transition gaps should be flagged.
        assert!(breaks[3], "expected break at gap 3 (cluster 0→1)");
        assert!(breaks[7], "expected break at gap 7 (cluster 1→2)");

        // Within-cluster gaps should be quiet. We allow one or two
        // false positives in the noisy jitter — the test checks the
        // signal-to-noise floor, not perfection.
        let within_breaks = breaks
            .iter()
            .enumerate()
            .filter(|(i, b)| **b && *i != 3 && *i != 7)
            .count();
        assert!(
            within_breaks <= 1,
            "too many false-positive within-cluster breaks: {within_breaks}"
        );
    }

    #[test]
    fn texttiling_flat_embeddings_emit_no_breaks() {
        // All identical embeddings → similarity is uniformly high,
        // depth signal is flat, no breaks should fire.
        let embeddings = vec![vec![0.5, 0.5, 0.5, 0.5]; 10];
        let breaks = detect_segment_boundaries(&embeddings, 3, 1.0);
        assert_eq!(breaks.len(), 9);
        assert!(
            breaks.iter().all(|b| !b),
            "flat signal should produce no breaks"
        );
    }

    #[test]
    fn texttiling_short_input_returns_empty() {
        assert!(detect_segment_boundaries(&[], 3, 1.0).is_empty());
        assert!(
            detect_segment_boundaries(&[vec![1.0, 0.0]], 3, 1.0).is_empty(),
            "single chunk has no gaps"
        );
    }

    #[test]
    fn texttiling_strict_threshold_silences_marginal_breaks() {
        // Same clusters as before but with a stricter k. The
        // cluster-transition gaps still fire (their depth is much
        // larger than within-cluster noise), but anything marginal
        // gets filtered out.
        let embeddings = synthetic_clusters(3, 4);
        let lax = detect_segment_boundaries(&embeddings, 3, 0.3);
        let strict = detect_segment_boundaries(&embeddings, 3, 1.5);
        let lax_count = lax.iter().filter(|b| **b).count();
        let strict_count = strict.iter().filter(|b| **b).count();
        assert!(
            strict_count <= lax_count,
            "stricter threshold must not produce more breaks (lax {lax_count} vs strict {strict_count})"
        );
    }

    // ── Motif extraction ─────────────────────────────────────────

    fn chunk(content: &str) -> TextChunk {
        TextChunk {
            content: content.to_string(),
            index: 0,
        }
    }

    #[test]
    fn motif_candidates_surface_recurring_word_across_many_chunks() {
        // "incurious" appears in 5 of 20 chunks (25%, well within
        // the 60% topicality ceiling) → must surface.
        // "hat" appears in 1 chunk → must not surface (df < 3 floor).
        // "the" appears in many chunks → must not surface (stoplisted).
        let mut chunks = vec![
            chunk("Winnie was incurious about the matter and went on with her work."),
            chunk("The professor walked alone, carrying his frail explosive device."),
            chunk("Stevie drew his circles, oblivious to the world around him."),
            chunk("Mrs Verloc remained incurious during the long evening at home."),
            chunk("The cab horse was whipped and Stevie's hat fell to the pavement."),
            chunk("Winnie's incurious eyes lighted on the broken figure of her brother."),
            chunk("The narrator notes Mrs Verloc was an incurious person by nature."),
            chunk("An incurious silence settled over the parlour after the explosion."),
        ];
        // Pad with topically-distinct chunks so the document is large
        // enough that "incurious" sits comfortably under the 60% ceiling.
        for i in 0..12 {
            chunks.push(chunk(&format!(
                "Topic {i} unrelated paragraph about unconnected events and \
                 separate matters with totally distinct vocabulary."
            )));
        }
        let cands = extract_motif_candidates(&chunks, 20);
        let terms: Vec<&str> = cands.iter().map(|c| c.term.as_str()).collect();
        assert!(
            terms.contains(&"incurious"),
            "expected 'incurious' in candidates; got {terms:?}"
        );
        assert!(
            !terms.contains(&"hat"),
            "single-chunk term must not surface"
        );
        assert!(!terms.contains(&"the"), "stoplisted word must not surface");
    }

    #[test]
    fn motif_candidates_drop_words_in_too_many_chunks() {
        // A word in >60% of chunks is topical not motivic. Build a
        // doc where "rust" appears in 8/10 chunks — should NOT surface
        // even though it's frequent.
        let mut chunks = Vec::new();
        for _ in 0..8 {
            chunks.push(chunk("This passage talks about rust programming."));
        }
        chunks.push(chunk("This passage talks about Python programming."));
        chunks.push(chunk("This passage talks about Java programming."));
        let cands = extract_motif_candidates(&chunks, 20);
        let terms: Vec<&str> = cands.iter().map(|c| c.term.as_str()).collect();
        assert!(
            !terms.contains(&"rust"),
            "topical term in 80% of chunks must be excluded; got {terms:?}"
        );
    }

    #[test]
    fn motif_candidates_handle_possessives_and_contractions() {
        // "winnie's" should normalize to "winnie" via the 's strip.
        // Need ≥10 chunks total so 3 occurrences stay under the 60% ceiling.
        let mut chunks = vec![
            chunk("Winnie's act was deliberate. Winnie's eyes were closed."),
            chunk("This is about Winnie's life and Winnie's choice."),
            chunk("The novel turns on Winnie's transformation through trauma."),
        ];
        for i in 0..8 {
            chunks.push(chunk(&format!(
                "Topic {i} unrelated paragraph with separate distinct vocabulary."
            )));
        }
        let cands = extract_motif_candidates(&chunks, 20);
        let terms: Vec<&str> = cands.iter().map(|c| c.term.as_str()).collect();
        assert!(
            terms.contains(&"winnie"),
            "possessive strip should normalise 'winnie's' → 'winnie'; got {terms:?}"
        );
        assert!(
            !terms.iter().any(|t| t.ends_with('\'')),
            "no candidate should retain a trailing apostrophe; got {terms:?}"
        );
    }

    #[test]
    fn parse_motif_classification_extracts_array_from_noisy_response() {
        // Model often wraps the JSON in extra text — parser must
        // skip prose and find the [...] span.
        let resp = r#"Sure! Here are the motifs: ["incurious", "circles", "professor"] — let me know if you want more."#;
        let set = parse_motif_classification(resp);
        assert_eq!(set.len(), 3);
        assert!(set.contains("incurious"));
        assert!(set.contains("circles"));
        assert!(set.contains("professor"));
    }

    #[test]
    fn parse_motif_classification_returns_empty_on_no_array() {
        let set = parse_motif_classification("the model refused to comply");
        assert!(set.is_empty());
    }

    #[test]
    fn parse_motif_classification_lowercases() {
        let set = parse_motif_classification(r#"["Incurious", "CIRCLES"]"#);
        assert!(set.contains("incurious"));
        assert!(set.contains("circles"));
    }

    #[test]
    fn cosine_similarity_basic_cases() {
        assert!((cosine_similarity(&[1.0, 0.0], &[1.0, 0.0]) - 1.0).abs() < 1e-6);
        assert!((cosine_similarity(&[1.0, 0.0], &[0.0, 1.0])).abs() < 1e-6);
        assert!((cosine_similarity(&[1.0, 0.0], &[-1.0, 0.0]) + 1.0).abs() < 1e-6);
        // Mismatched length → 0.0 sentinel.
        assert_eq!(cosine_similarity(&[1.0], &[1.0, 0.0]), 0.0);
        // Zero magnitude → 0.0 sentinel.
        assert_eq!(cosine_similarity(&[0.0, 0.0], &[1.0, 1.0]), 0.0);
    }
}
