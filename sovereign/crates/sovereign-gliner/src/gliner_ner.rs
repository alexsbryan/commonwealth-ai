// SPDX-License-Identifier: AGPL-3.0-or-later
//! Per-chunk named entity recognition via GLiNER (pure Rust through
//! `gline-rs`).
//!
//! Spec: `sovereign/docs/specs/CONV_TIERED_PORT.md` §"Phase 1 —
//! GliNER per-chunk entities".
//!
//! Why this exists: RAPTOR's cluster-summary prompt extracts a
//! handful of entities per leaf, missing long-tail mentions and
//! returning empty sets for Tiny convs (151 of 576 in
//! conversations-anthropic). A dedicated NER pass produces ~10x
//! denser coverage (~10 mentions/chunk) with typed labels
//! (Person/Organization/Work/Location/Event), validated 2026-05-23
//! against the same corpus.
//!
//! Architecture:
//! - `gline-rs` (Apache-2.0, v1.x) wraps `ort` for ONNX inference.
//! - Model + tokenizer files load from `~/.sovereign/models/gliner/`
//!   on first instantiation. Default: `gliner_small-v2.1` (591MB on
//!   disk, measured 2026-07-30 — the ~150MB figure previously here was
//!   wrong; SP1 measured ~424 words/s on SEP doc chunks in-process).
//!   Operator can drop in a larger model and point
//!   `SOVEREIGN_GLINER_MODEL_DIR` at it.
//! - Threshold default `0.6` (validation showed 0.5 admitted noise
//!   on common nouns; 0.6 keeps long-tail names like
//!   "Park Chunghee" while dropping "joy" / "desire" false positives).
//! - Label set excludes the `Concept` label — too generic, accounted
//!   for 35% of mentions in validation with low precision. Salient
//!   concepts surface via `Organization` / `Work` when they
//!   genuinely matter.
//! - Conv role markers (`### [date] user`, `### [date] assistant`)
//!   are stripped before inference so GliNER doesn't tag the literal
//!   word "assistant" as a Person (live false positive observed in
//!   all 10 validation convs).

use std::path::PathBuf;
use std::sync::Mutex;

use gliner::model::input::text::TextInput;
use gliner::model::params::Parameters;
use gliner::model::pipeline::span::SpanMode;
use gliner::model::GLiNER;
use orp::params::RuntimeParameters;
use regex::Regex;
use sovereign_core::conv_tiered::ChunkEntityRow;
use sovereign_core::error::{Error, Result};

/// Default extraction threshold. Below this, GliNER's softmax score
/// is too low to trust — most below-threshold "mentions" in
/// validation were common nouns ("joy", "concerns") rather than
/// true named entities.
pub const DEFAULT_THRESHOLD: f32 = 0.6;

/// Label set the extractor passes to GliNER at inference time.
/// `Concept` deliberately omitted (high noise rate; see module
/// docstring). The remaining labels cover the entity types the
/// conv corpus actually contains in production data.
pub const DEFAULT_LABELS: &[&str] = &["Person", "Organization", "Work", "Location", "Event"];

/// Label set for the retrieval-side CONCEPT extraction pass (see
/// [`EntityExtractor::extract_concepts`](sovereign_core::traits::EntityExtractor::extract_concepts)).
/// Deliberately a SEPARATE, single-label pass rather than an addition to
/// [`DEFAULT_LABELS`]: GLiNER does joint inference over the provided
/// labels, so folding `Concept` into the 5-label set would shift the
/// other labels' spans and perturb the tuned seed-confirmation filter —
/// and the conv-ingestion path excludes `Concept` for precision (see
/// module docstring). Isolating it here keeps both callers unperturbed.
pub const CONCEPT_LABELS: &[&str] = &["Concept"];

/// Score cutoff for the concept pass. Set below the concepts a question
/// actually names (probe 2026-07-17 on the wiki bank: `determinism` 0.80,
/// `uncertainty principle` 0.72, `globalization` 0.83, `European
/// colonialism` 0.55, `Enlightenment` 0.56) and above the low-confidence
/// noise a bare `Concept` label admits. Remaining precision is recovered
/// downstream by the FTS-exact-title obligation gate, which only fires
/// for a concept that has a canonical article.
pub const CONCEPT_THRESHOLD: f32 = 0.5;

/// Default model id. Maps to a directory inside `MODELS_ROOT`; the
/// files inside it depend on the generation — see [`model_spec`].
pub const DEFAULT_MODEL_ID: &str = "gliner_small-v2.1";

/// The GLiNER2 base export evaluated in SP1 — a monolithic
/// encoder+span-head graph, driven bare on `ort` (no gline-rs).
pub const GLINER2_MODEL_ID: &str = "gliner2-base-v1-onnx";

/// Which GLiNER generation a model id belongs to.
///
/// This is a closed set on purpose (ARCH_PRINCIPLES §2): each variant
/// implies a different input contract and a different loader, so a
/// generation the code cannot drive must not be nameable in config.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GlinerGeneration {
    /// gline-rs stack, entities only.
    V1,
    /// Bare-`ort` schema-driven export: entities, types, typed slots.
    V2,
}

/// Where a GLiNER model lives on HuggingFace and how its files are laid
/// out on disk.
///
/// Both varied across generations and both were hardcoded to v1 before
/// 2026-08-02: the org was fixed to `onnx-community` and the graph to
/// `onnx/model.onnx`, so a GLiNER2 export (`lion-ai/…`, monolithic
/// `model.onnx` at the root) could not be fetched or resolved at all.
#[derive(Debug, Clone, Copy)]
pub struct GlinerModelSpec {
    /// HuggingFace org that owns the repo.
    pub hf_org: &'static str,
    /// Tokenizer path, relative to both the HF repo and the local dir.
    pub tokenizer_rel: &'static str,
    /// ONNX graph path, relative to both the HF repo and the local dir.
    pub onnx_rel: &'static str,
    pub generation: GlinerGeneration,
}

const V1_LAYOUT: GlinerModelSpec = GlinerModelSpec {
    hf_org: "onnx-community",
    tokenizer_rel: "tokenizer.json",
    onnx_rel: "onnx/model.onnx",
    generation: GlinerGeneration::V1,
};

const V2_LAYOUT: GlinerModelSpec = GlinerModelSpec {
    hf_org: "lion-ai",
    tokenizer_rel: "tokenizer.json",
    onnx_rel: "model.onnx",
    generation: GlinerGeneration::V2,
};

/// Model ids whose layout is known exactly.
const KNOWN_MODELS: &[(&str, &GlinerModelSpec)] = &[
    (DEFAULT_MODEL_ID, &V1_LAYOUT),
    (GLINER2_MODEL_ID, &V2_LAYOUT),
];

/// Resolve a model id to its HF coordinates + on-disk layout.
///
/// Unknown ids fall back to the v1 layout — which is exactly the
/// behaviour every caller had before this table existed, so a
/// hand-passed `--model-id` for some other `onnx-community` GLiNER v1
/// export keeps working. The fallback is **announced, not silent**
/// (`tracing::warn!`): a GLiNER2-generation export resolved as v1 fails
/// later with a confusing missing-file error, and the warning is what
/// points at the real cause.
pub fn model_spec(model_id: &str) -> &'static GlinerModelSpec {
    for (id, spec) in KNOWN_MODELS {
        if *id == model_id {
            return spec;
        }
    }
    tracing::warn!(
        model_id,
        assumed_org = V1_LAYOUT.hf_org,
        assumed_onnx = V1_LAYOUT.onnx_rel,
        known = ?KNOWN_MODELS.iter().map(|(id, _)| *id).collect::<Vec<_>>(),
        "gliner: unknown model id — assuming the v1 layout. If this is a \
         GLiNER2-generation export, add it to KNOWN_MODELS instead."
    );
    &V1_LAYOUT
}

/// Returns the configured models root, falling back to
/// `~/.sovereign/models/gliner` when the env var is unset.
pub fn models_root() -> PathBuf {
    if let Ok(p) = std::env::var("SOVEREIGN_GLINER_MODEL_DIR") {
        return PathBuf::from(p);
    }
    dirs::home_dir()
        .map(|h| h.join(".sovereign").join("models").join("gliner"))
        .unwrap_or_else(|| PathBuf::from(".sovereign/models/gliner"))
}

/// Resolve `(tokenizer_path, model_path)` for a model id, validating
/// both files exist. Returns a descriptive error if the layout is
/// not what gline-rs expects — gives the operator a clear "go
/// download this model" message rather than a cryptic ort error.
pub fn resolve_model_paths(model_id: &str) -> Result<(PathBuf, PathBuf)> {
    let spec = model_spec(model_id);
    let root = models_root().join(model_id);
    let tokenizer = root.join(spec.tokenizer_rel);
    let model = root.join(spec.onnx_rel);
    if !tokenizer.is_file() {
        return Err(Error::Storage(format!(
            "GliNER tokenizer not found at {}\n\
             Download model files from huggingface.co/{}/{} into\n\
             {}/ — must contain {} + {}",
            tokenizer.display(),
            spec.hf_org,
            model_id,
            root.display(),
            spec.tokenizer_rel,
            spec.onnx_rel,
        )));
    }
    if !model.is_file() {
        return Err(Error::Storage(format!(
            "GliNER ONNX model not found at {}\n\
             Expected {}/{} — see huggingface.co/{}/{}",
            model.display(),
            root.display(),
            spec.onnx_rel,
            spec.hf_org,
            model_id,
        )));
    }
    Ok((tokenizer, model))
}

/// One extracted entity mention with character offsets into the
/// preprocessed (role-marker-stripped) chunk text. Use the
/// `original_offsets_from_processed` helper to map back to offsets
/// in the raw chunk content for highlight rendering.
#[derive(Debug, Clone, PartialEq)]
pub struct EntityMention {
    pub text: String,
    pub label: String,
    pub char_start: usize,
    pub char_end: usize,
    pub score: f32,
}

impl EntityMention {
    /// Promote a stack of mentions into persisted `ChunkEntityRow`s
    /// for one chunk. Callers stamp `extracted_at` from a single
    /// timestamp so all rows in a batch share the same provenance.
    pub fn into_row(
        self,
        corpus_id: &str,
        chunk_id: u64,
        conv_uuid: Option<&str>,
        extracted_at: i64,
    ) -> ChunkEntityRow {
        ChunkEntityRow {
            corpus_id: corpus_id.to_string(),
            chunk_id,
            text: self.text,
            label: self.label,
            char_start: self.char_start as i64,
            char_end: self.char_end as i64,
            score: self.score as f64,
            conv_uuid: conv_uuid.map(|s| s.to_string()),
            extracted_at,
        }
    }
}

/// gline-rs's `GLiNER<SpanMode>` model wrapped behind a `Mutex` so
/// the inference call can stay `&self` from any caller while still
/// honouring the underlying model's `&mut self` requirement on
/// `inference`. The Mutex contention is per-corpus extraction (one
/// chunk at a time on a single CPU), not a concurrency hotspot.
pub struct GlinerExtractor {
    model: Mutex<GLiNER<SpanMode>>,
    pub model_id: String,
    pub labels: Vec<String>,
    pub threshold: f32,
    role_marker_re: Regex,
}

/// Single source of truth for the conversation role-marker pattern.
///
/// Two role-marker formats in production:
///  - claude.ai zip export (conv-anthropic): `### [2025-08-05 14:22] user`
///  - Sovereign-internal chat history (conversation-history): `[user]` /
///    `[assistant]` inline markers
///  - conversations-personal: same as conv-anthropic
///
/// Either format must strip so GliNER doesn't tag the literal role word
/// as a Person. The constructor and the unit tests both build the regex
/// from here, so the test can never silently diverge from production.
pub(crate) fn role_marker_regex() -> Regex {
    Regex::new(
        r"(?m)(?:^###\s+\[[^\]]+\]\s+(?:user|assistant|system)\s*$|\[(?:user|assistant|system)\])",
    )
    .expect("static regex compiles")
}

/// Strip conversation role markers from `raw`. A pure function of
/// (text, regex) with no model state — which is exactly why it can be
/// unit-tested directly, without constructing a `GlinerExtractor` (and
/// thus without fabricating a `GLiNER` model the test never uses).
pub(crate) fn strip_role_markers(raw: &str, re: &Regex) -> String {
    re.replace_all(raw, "").into_owned()
}

impl GlinerExtractor {
    /// Construct an extractor with the default model id, labels, and
    /// threshold. Reads model files from `models_root()`.
    pub fn new_default() -> Result<Self> {
        Self::new(DEFAULT_MODEL_ID, DEFAULT_LABELS, DEFAULT_THRESHOLD)
    }

    /// Custom construction. `labels` is the set passed to GliNER at
    /// inference; `threshold` is the per-span score cutoff.
    pub fn new(model_id: &str, labels: &[&str], threshold: f32) -> Result<Self> {
        // `resolve_model_paths` can now FIND a GLiNER2 export, but this
        // constructor drives gline-rs, which implements v1's input
        // contract only (a GLiNER2 graph wants
        // `text_positions`/`schema_positions`/`span_idx`). Handing one to
        // `GLiNER::<SpanMode>::new` fails deep inside ort with a shape
        // error that reads like a corrupt download. Refuse by generation
        // instead, and name the replacement.
        let spec = model_spec(model_id);
        if spec.generation == GlinerGeneration::V2 {
            return Err(Error::Storage(format!(
                "{model_id} is a GLiNER2-generation export; GlinerExtractor \
                 drives the gline-rs v1 stack and cannot run it. Use the \
                 bare-ort GLiNER2 backend (see \
                 sovereign-gliner/examples/gliner2_probe.rs, SP1)."
            )));
        }
        let (tokenizer_path, model_path) = resolve_model_paths(model_id)?;
        let model = GLiNER::<SpanMode>::new(
            Parameters::default(),
            RuntimeParameters::default(),
            tokenizer_path
                .to_str()
                .ok_or_else(|| Error::Storage("non-utf8 tokenizer path".into()))?,
            model_path
                .to_str()
                .ok_or_else(|| Error::Storage("non-utf8 model path".into()))?,
        )
        .map_err(|e| Error::Storage(format!("GLiNER::new: {e}")))?;
        Ok(Self {
            model: Mutex::new(model),
            model_id: model_id.to_string(),
            labels: labels.iter().map(|s| s.to_string()).collect(),
            threshold,
            role_marker_re: role_marker_regex(),
        })
    }

    /// Strip conv-format role markers + collapse to plain prose.
    /// Markers look like `### [2025-12-08 20:36] user` — they make
    /// GliNER tag the role word as a Person. Replacing with a blank
    /// line preserves character offsets within message bodies
    /// (offsets reported by GliNER point into the stripped text,
    /// not the raw chunk — callers using offsets for highlighting
    /// need to be aware).
    pub fn preprocess(&self, raw: &str) -> String {
        strip_role_markers(raw, &self.role_marker_re)
    }

    /// Extract entities from one chunk. Returns mentions
    /// already-threshold-filtered + deduped within the chunk
    /// (case-insensitive `(text, label)` collision wins by highest
    /// score).
    pub fn extract(&self, raw_chunk_text: &str) -> Result<Vec<EntityMention>> {
        let labels_ref: Vec<&str> = self.labels.iter().map(|s| s.as_str()).collect();
        self.extract_labeled(raw_chunk_text, &labels_ref, self.threshold)
    }

    /// Core single-chunk extraction over an EXPLICIT label set +
    /// threshold. [`extract`](Self::extract) runs it with the slot's
    /// configured 5 labels; the retrieval `Concept` pass runs it with
    /// [`CONCEPT_LABELS`] / [`CONCEPT_THRESHOLD`]. gline-rs takes the
    /// label set per inference call, so both share one model and one
    /// code path — no second extractor, no divergent dedup logic.
    pub fn extract_labeled(
        &self,
        raw_chunk_text: &str,
        labels: &[&str],
        threshold: f32,
    ) -> Result<Vec<EntityMention>> {
        let processed = self.preprocess(raw_chunk_text);
        if processed.trim().is_empty() {
            return Ok(Vec::new());
        }
        let texts = vec![processed.as_str()];
        let input = TextInput::from_str(&texts, labels)
            .map_err(|e| Error::Storage(format!("TextInput::from_str: {e}")))?;
        let guard = self
            .model
            .lock()
            .map_err(|_| Error::Storage("gliner mutex poisoned".into()))?;
        let output = guard
            .inference(input)
            .map_err(|e| Error::Storage(format!("GLiNER::inference: {e}")))?;
        drop(guard);
        let mut mentions: Vec<EntityMention> = Vec::new();
        for spans in output.spans {
            for span in spans {
                let prob = span.probability();
                if prob < threshold {
                    continue;
                }
                let (char_start, char_end) = span.offsets();
                mentions.push(EntityMention {
                    text: normalize_mention_text(span.text()),
                    label: span.class().to_string(),
                    char_start,
                    char_end,
                    score: prob,
                });
            }
        }
        Ok(crate::labeled::dedupe_strongest(mentions))
    }

    /// Extract entities from many chunks at once, returning one
    /// `Vec<EntityMention>` per input in input order. Single
    /// GliNER batch call when possible — much faster than looping
    /// `extract` for a many-chunk corpus pass.
    pub fn extract_batch(&self, raw_chunks: &[&str]) -> Result<Vec<Vec<EntityMention>>> {
        if raw_chunks.is_empty() {
            return Ok(Vec::new());
        }
        let processed: Vec<String> = raw_chunks.iter().map(|s| self.preprocess(s)).collect();
        let processed_refs: Vec<&str> = processed.iter().map(|s| s.as_str()).collect();
        let labels_ref: Vec<&str> = self.labels.iter().map(|s| s.as_str()).collect();
        let input = TextInput::from_str(&processed_refs, &labels_ref)
            .map_err(|e| Error::Storage(format!("TextInput::from_str: {e}")))?;
        let guard = self
            .model
            .lock()
            .map_err(|_| Error::Storage("gliner mutex poisoned".into()))?;
        let output = guard
            .inference(input)
            .map_err(|e| Error::Storage(format!("GLiNER::inference: {e}")))?;
        drop(guard);
        let mut out: Vec<Vec<EntityMention>> = vec![Vec::new(); raw_chunks.len()];
        // gline-rs's `output.spans` is `Vec<Vec<Span>>` with the same
        // ordering as the input texts. `span.sequence()` indexes back
        // into the input vector — defensive use just in case the
        // outer vec order ever drifts.
        for (i, spans) in output.spans.iter().enumerate() {
            let mut chunk_mentions: Vec<EntityMention> = Vec::new();
            for span in spans {
                let prob = span.probability();
                if prob < self.threshold {
                    continue;
                }
                let (char_start, char_end) = span.offsets();
                chunk_mentions.push(EntityMention {
                    text: normalize_mention_text(span.text()),
                    label: span.class().to_string(),
                    char_start,
                    char_end,
                    score: prob,
                });
            }
            out[i] = crate::labeled::dedupe_strongest(chunk_mentions);
        }
        Ok(out)
    }
}

/// v1 on the ingest seam. `extract_mentions_batch` is overridden
/// because gline-rs batches natively — one `inference` call for N
/// chunks, which is where v1's throughput comes from; the trait's
/// looping default would forfeit it.
impl crate::labeled::LabeledEntityExtractor for GlinerExtractor {
    fn model_id(&self) -> &str {
        &self.model_id
    }

    fn labels(&self) -> Vec<String> {
        self.labels.clone()
    }

    fn threshold(&self) -> f32 {
        self.threshold
    }

    fn extract_mentions(&self, text: &str) -> Result<Vec<EntityMention>> {
        self.extract(text)
    }

    fn extract_mentions_batch(&self, texts: &[&str]) -> Result<Vec<Vec<EntityMention>>> {
        self.extract_batch(texts)
    }
}

/// Implementation of the `sovereign-core::traits::EntityExtractor`
/// trait. Wraps `GlinerExtractor::extract` and dedupes by entity
/// text (lower-cased). The trait elides the label because
/// retrieval-side scoring only needs the entity STRING for
/// jaccard overlap, not its NER type.
///
/// Errors from `extract` (rare, typically ORT runtime issues) are
/// downgraded to an empty Vec — entity-aware retrieval falls back
/// to pure cosine on that turn instead of crashing the synthesis
/// path. The retrieval call sites already log soft-failures via
/// `tracing::debug!`.
impl sovereign_core::traits::EntityExtractor for GlinerExtractor {
    fn extract_entities(&self, text: &str) -> Vec<String> {
        dedup_mention_texts(self.extract(text).ok())
    }

    /// Run the dedicated `Concept` pass (see [`CONCEPT_LABELS`]). Errors
    /// (rare ORT runtime issues) degrade to no concepts — retrieval's
    /// obligation lane just doesn't gain the concept articles this turn,
    /// exactly as when the model isn't installed.
    fn extract_concepts(&self, text: &str) -> Vec<String> {
        dedup_mention_texts(
            self.extract_labeled(text, CONCEPT_LABELS, CONCEPT_THRESHOLD)
                .ok(),
        )
    }
}

/// Lower-case, dedupe (preserving first-seen order) the text of a set of
/// mentions. Shared by `extract_entities` and `extract_concepts` so the
/// two produce identically-shaped output. `None` (an extraction error)
/// yields an empty Vec.
fn dedup_mention_texts(mentions: Option<Vec<EntityMention>>) -> Vec<String> {
    let Some(mentions) = mentions else {
        return Vec::new();
    };
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::with_capacity(mentions.len());
    for m in mentions {
        let key = m.text.to_lowercase();
        if seen.insert(key.clone()) {
            out.push(key);
        }
    }
    out
}

/// Boot-critical-path-free wrapper around [`GlinerExtractor`].
///
/// Loading the GLiNER model (`GlinerExtractor::new_default`) costs ~950ms —
/// on the desktop that was roughly half of the whole warm boot, all spent
/// synchronously before `backend-ready` fires. This decorator moves that
/// load onto a background thread at construction and installs immediately,
/// so bootstrap pays only the (cheap) `probe_model_available` check.
///
/// Until the background load completes, `extract_entities` returns an empty
/// `Vec` — the exact same soft-fallback the retrieval path already takes
/// when GLiNER isn't installed at all (it degrades to cosine + MMR history
/// retrieval, see `Runtime::maybe_retrieve_relevant_history`). In practice
/// the model is warm within ~1s of boot — long before a user reads the
/// freshly-loaded UI and types a first query — so entity-aware retrieval is
/// effectively always available by the time it's exercised. A load failure
/// is logged once and leaves the extractor permanently in fallback mode,
/// identical to today's `new_default` error branch.
pub struct LazyGlinerExtractor {
    inner: std::sync::Arc<std::sync::OnceLock<GlinerExtractor>>,
}

impl LazyGlinerExtractor {
    /// Install immediately and warm the default model on a background
    /// thread. Callers should still gate construction on
    /// [`probe_model_available`] so a machine without the model doesn't
    /// spawn a thread only to fail.
    pub fn new_default_deferred() -> Self {
        let inner = std::sync::Arc::new(std::sync::OnceLock::new());
        let slot = std::sync::Arc::clone(&inner);
        let spawned = std::thread::Builder::new()
            .name("gliner-warm".into())
            .spawn(move || {
                let t = std::time::Instant::now();
                match GlinerExtractor::new_default() {
                    Ok(g) => {
                        let _ = slot.set(g);
                        tracing::info!(
                            elapsed_ms = t.elapsed().as_millis() as u64,
                            "GLiNER entity extractor warmed (background)"
                        );
                    }
                    Err(e) => {
                        tracing::warn!(
                            error = %e,
                            "GLiNER background load failed; entity-aware retrieval disabled (cosine+MMR fallback)"
                        );
                    }
                }
            });
        if let Err(e) = spawned {
            tracing::warn!(error = %e, "GLiNER background warm thread failed to spawn; entity-aware retrieval disabled");
        }
        Self { inner }
    }
}

impl sovereign_core::traits::EntityExtractor for LazyGlinerExtractor {
    fn extract_entities(&self, text: &str) -> Vec<String> {
        match self.inner.get() {
            Some(g) => g.extract_entities(text),
            // Not warm yet (or load failed): same soft-fallback as an
            // uninstalled model — the retrieval path degrades to cosine+MMR.
            None => Vec::new(),
        }
    }

    fn extract_concepts(&self, text: &str) -> Vec<String> {
        match self.inner.get() {
            Some(g) => g.extract_concepts(text),
            // Not warm yet (or load failed): no concept obligations this
            // turn, same soft-fallback as an uninstalled model.
            None => Vec::new(),
        }
    }
}

/// Collapse internal whitespace + trim. GliNER's span text
/// occasionally crosses newlines in the source (e.g. multi-line
/// "Jonathan\nSwift") because the model operates on the chunk
/// character stream including the chat-message internal line
/// breaks. Stored entity strings should be human-clean.
pub(crate) fn normalize_mention_text(raw: &str) -> String {
    raw.split_whitespace().collect::<Vec<_>>().join(" ")
}

pub use sovereign_core::time::unix_now as now_unix;

/// Probe-style helper: returns true if the configured model is
/// installed and the extractor can be loaded. Useful for CLI
/// preflight before kicking off a long extraction.
pub fn probe_model_available(model_id: &str) -> bool {
    resolve_model_paths(model_id).is_ok()
}

/// Download model files for a given GliNER model id from
/// `huggingface.co/<org>/<model_id>`, where the org and the file layout
/// both come from [`model_spec`] — they are per-generation, not
/// constants. Writes into `models_root().join(model_id)`. Skips files
/// already present — idempotent.
///
/// Reports progress via the `on_progress` callback (bytes downloaded,
/// total bytes). The desktop wires this to a status pill; the CLI
/// renders a percentage line. Long-running — typical model is
/// ~600MB.
pub async fn download_model(
    model_id: &str,
    on_progress: impl Fn(&str, u64, u64) + Send + Sync,
) -> Result<()> {
    let spec = model_spec(model_id);
    let root = models_root().join(model_id);

    let files = [
        (spec.tokenizer_rel, root.join(spec.tokenizer_rel)),
        (spec.onnx_rel, root.join(spec.onnx_rel)),
    ];
    // Create each file's parent rather than a hardcoded `onnx/` — the
    // GLiNER2 export puts the graph at the root, so that directory does
    // not exist for every generation.
    for (_, local_path) in &files {
        if let Some(parent) = local_path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| Error::Storage(format!("create_dir_all {}: {e}", parent.display())))?;
        }
    }

    let client = reqwest::Client::builder()
        .user_agent("sovereign-tools/gliner-fetch (maintainer@example.com)")
        .timeout(std::time::Duration::from_secs(900))
        .build()
        .map_err(|e| Error::Storage(format!("reqwest::Client build: {e}")))?;

    for (remote_rel, local_path) in &files {
        if local_path.is_file() {
            on_progress(remote_rel, 0, 0);
            continue;
        }
        let url = format!(
            "https://huggingface.co/{}/{model_id}/resolve/main/{remote_rel}",
            spec.hf_org
        );
        let mut resp = client
            .get(&url)
            .send()
            .await
            .map_err(|e| Error::Storage(format!("fetch {url}: {e}")))?;
        if !resp.status().is_success() {
            return Err(Error::Storage(format!(
                "fetch {url}: HTTP {}",
                resp.status()
            )));
        }
        let total = resp.content_length().unwrap_or(0);
        let tmp_path = local_path.with_extension("downloading");
        let mut file = tokio::fs::File::create(&tmp_path)
            .await
            .map_err(|e| Error::Storage(format!("create {}: {e}", tmp_path.display())))?;
        let mut downloaded: u64 = 0;
        use tokio::io::AsyncWriteExt;
        while let Some(chunk) = resp
            .chunk()
            .await
            .map_err(|e| Error::Storage(format!("read chunk: {e}")))?
        {
            file.write_all(&chunk)
                .await
                .map_err(|e| Error::Storage(format!("write: {e}")))?;
            downloaded += chunk.len() as u64;
            on_progress(remote_rel, downloaded, total);
        }
        file.flush()
            .await
            .map_err(|e| Error::Storage(format!("flush: {e}")))?;
        drop(file);
        std::fs::rename(&tmp_path, local_path).map_err(|e| {
            Error::Storage(format!(
                "rename {} → {}: {e}",
                tmp_path.display(),
                local_path.display()
            ))
        })?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    // `preprocess` is a thin wrapper over `strip_role_markers`, which
    // is a pure function of (text, regex) with no model state. Test it
    // directly: constructing a `GlinerExtractor` here would require a
    // real `GLiNER` model (the model field has no valid zero bit
    // pattern — `mem::zeroed()` aborts under the rustc validity check),
    // and the role-marker logic never touches the model anyway.
    #[test]
    fn anthropic_role_markers_get_stripped() {
        let re = role_marker_regex();
        let raw = "### [2025-12-08 20:36] user\nWhat do you think about Borges?\n### [2025-12-08 20:37] assistant\nBorges is a literary giant.";
        let processed = strip_role_markers(raw, &re);
        assert!(!processed.contains("user"));
        assert!(!processed.contains("assistant"));
        assert!(processed.contains("Borges?"));
        assert!(processed.contains("literary giant"));
    }

    #[test]
    fn sovereign_internal_role_markers_get_stripped() {
        let re = role_marker_regex();
        // conversation-history uses inline [user]/[assistant] markers.
        let raw = "[user] Tell me about Borges and labyrinths.\n\n[assistant] Borges treats labyrinths as a metaphor for coexistence.";
        let processed = strip_role_markers(raw, &re);
        assert!(!processed.contains("[user]"));
        assert!(!processed.contains("[assistant]"));
        assert!(processed.contains("Borges"));
        assert!(processed.contains("labyrinths"));
    }

    /// `SOVEREIGN_GLINER_MODEL_DIR` is process-global state, and the
    /// default test runner is parallel — the two tests below both
    /// mutate it and race without this lock (observed: the first
    /// test reading the second's value mid-run). A poisoned lock is
    /// fine to reuse: the var is reset at the top of each test.
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn models_root_honors_env_var() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        std::env::set_var("SOVEREIGN_GLINER_MODEL_DIR", "/tmp/custom-models");
        let root = models_root();
        assert_eq!(root, PathBuf::from("/tmp/custom-models"));
        std::env::remove_var("SOVEREIGN_GLINER_MODEL_DIR");
    }

    #[test]
    fn resolve_model_paths_errors_clearly_when_missing() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        std::env::set_var("SOVEREIGN_GLINER_MODEL_DIR", "/tmp/definitely-not-here");
        let err = resolve_model_paths("gliner_small-v2.1");
        assert!(err.is_err());
        let msg = format!("{}", err.unwrap_err());
        assert!(msg.contains("tokenizer.json"), "msg = {msg}");
        std::env::remove_var("SOVEREIGN_GLINER_MODEL_DIR");
    }

    #[test]
    fn model_spec_maps_generation_org_and_layout() {
        let v1 = model_spec(DEFAULT_MODEL_ID);
        assert_eq!(v1.generation, GlinerGeneration::V1);
        assert_eq!(v1.hf_org, "onnx-community");
        assert_eq!(v1.onnx_rel, "onnx/model.onnx");

        let v2 = model_spec(GLINER2_MODEL_ID);
        assert_eq!(v2.generation, GlinerGeneration::V2);
        assert_eq!(v2.hf_org, "lion-ai");
        assert_eq!(v2.onnx_rel, "model.onnx");

        // Unknown ids keep the behaviour every caller had before the
        // table existed — v1 org + v1 layout, announced via `warn!`.
        let unknown = model_spec("some-other-gliner-export");
        assert_eq!(unknown.generation, GlinerGeneration::V1);
        assert_eq!(unknown.hf_org, "onnx-community");
    }

    /// The regression the spec table exists to prevent: before
    /// 2026-08-02 the ONNX path was hardcoded to `onnx/model.onnx`, so a
    /// correctly-installed GLiNER2 export — whose graph sits at the root
    /// — resolved to a missing file and was unusable.
    #[test]
    fn gliner2_resolves_root_level_onnx_not_the_v1_subdir() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let root = std::env::temp_dir().join(format!(
            "sovereign-gliner-v2-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let model_dir = root.join(GLINER2_MODEL_ID);
        std::fs::create_dir_all(&model_dir).unwrap();
        std::fs::write(model_dir.join("tokenizer.json"), b"{}").unwrap();
        std::fs::write(model_dir.join("model.onnx"), b"onnx").unwrap();

        std::env::set_var("SOVEREIGN_GLINER_MODEL_DIR", &root);
        let resolved = resolve_model_paths(GLINER2_MODEL_ID);
        std::env::remove_var("SOVEREIGN_GLINER_MODEL_DIR");

        let (tokenizer, onnx) = resolved.expect("GLiNER2 layout should resolve");
        assert_eq!(onnx, model_dir.join("model.onnx"));
        assert_eq!(tokenizer, model_dir.join("tokenizer.json"));
        std::fs::remove_dir_all(&root).ok();
    }

    /// The other half: v1's layout must NOT be loosened to accept a
    /// root-level graph, or a half-installed v1 dir would look valid.
    #[test]
    fn v1_still_requires_its_onnx_subdir() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let root = std::env::temp_dir().join(format!(
            "sovereign-gliner-v1-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let model_dir = root.join(DEFAULT_MODEL_ID);
        std::fs::create_dir_all(&model_dir).unwrap();
        std::fs::write(model_dir.join("tokenizer.json"), b"{}").unwrap();
        // Graph at the ROOT — the GLiNER2 shape, wrong for v1.
        std::fs::write(model_dir.join("model.onnx"), b"onnx").unwrap();

        std::env::set_var("SOVEREIGN_GLINER_MODEL_DIR", &root);
        let resolved = resolve_model_paths(DEFAULT_MODEL_ID);
        std::env::remove_var("SOVEREIGN_GLINER_MODEL_DIR");

        assert!(
            resolved.is_err(),
            "v1 must still demand onnx/model.onnx, got {resolved:?}"
        );
        std::fs::remove_dir_all(&root).ok();
    }

    /// A GLiNER2 graph must be refused BY GENERATION, before gline-rs
    /// gets it — otherwise the failure surfaces as an ort shape error
    /// that reads like a corrupt download. Uses a fully-installed v2
    /// layout so the refusal cannot be confused with a missing file.
    #[test]
    fn gliner_extractor_refuses_a_v2_model_by_generation() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let root = std::env::temp_dir().join(format!(
            "sovereign-gliner-refuse-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let model_dir = root.join(GLINER2_MODEL_ID);
        std::fs::create_dir_all(&model_dir).unwrap();
        std::fs::write(model_dir.join("tokenizer.json"), b"{}").unwrap();
        std::fs::write(model_dir.join("model.onnx"), b"onnx").unwrap();

        std::env::set_var("SOVEREIGN_GLINER_MODEL_DIR", &root);
        let built = GlinerExtractor::new(GLINER2_MODEL_ID, DEFAULT_LABELS, DEFAULT_THRESHOLD);
        std::env::remove_var("SOVEREIGN_GLINER_MODEL_DIR");

        let err = format!("{}", built.err().expect("v2 model must be refused"));
        assert!(err.contains("GLiNER2"), "err = {err}");
        assert!(
            !err.contains("GLiNER::new"),
            "must refuse before reaching gline-rs: {err}"
        );
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn missing_gliner2_error_names_its_own_org_not_v1s() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        std::env::set_var("SOVEREIGN_GLINER_MODEL_DIR", "/tmp/definitely-not-here");
        let err = resolve_model_paths(GLINER2_MODEL_ID).unwrap_err();
        std::env::remove_var("SOVEREIGN_GLINER_MODEL_DIR");
        let msg = format!("{err}");
        assert!(msg.contains("lion-ai"), "msg = {msg}");
        assert!(
            !msg.contains("onnx-community"),
            "error must not send the operator to the v1 org: {msg}"
        );
    }
}
