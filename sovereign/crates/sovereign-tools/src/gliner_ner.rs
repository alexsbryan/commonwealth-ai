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
//!   on first instantiation. Default: `gliner_small-v2.1` (~150MB,
//!   ~10k tok/s CPU). Operator can drop in a larger model and point
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

#![cfg(feature = "gliner-ner")]

use std::path::PathBuf;
use std::sync::Mutex;

use chrono::Utc;
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

/// Default model id. Maps to a directory inside `MODELS_ROOT`
/// containing `tokenizer.json` + `onnx/model.onnx`.
pub const DEFAULT_MODEL_ID: &str = "gliner_small-v2.1";

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
    let root = models_root().join(model_id);
    let tokenizer = root.join("tokenizer.json");
    let model = root.join("onnx").join("model.onnx");
    if !tokenizer.is_file() {
        return Err(Error::Storage(format!(
            "GliNER tokenizer not found at {}\n\
             Download model files from huggingface.co/onnx-community/{} into\n\
             {}/ — must contain tokenizer.json + onnx/model.onnx",
            tokenizer.display(),
            model_id,
            root.display(),
        )));
    }
    if !model.is_file() {
        return Err(Error::Storage(format!(
            "GliNER ONNX model not found at {}\n\
             Expected {}/onnx/model.onnx — see huggingface.co/onnx-community/{}",
            model.display(),
            root.display(),
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
fn role_marker_regex() -> Regex {
    Regex::new(
        r"(?m)(?:^###\s+\[[^\]]+\]\s+(?:user|assistant|system)\s*$|\[(?:user|assistant|system)\])",
    )
    .expect("static regex compiles")
}

/// Strip conversation role markers from `raw`. A pure function of
/// (text, regex) with no model state — which is exactly why it can be
/// unit-tested directly, without constructing a `GlinerExtractor` (and
/// thus without fabricating a `GLiNER` model the test never uses).
fn strip_role_markers(raw: &str, re: &Regex) -> String {
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
        let processed = self.preprocess(raw_chunk_text);
        if processed.trim().is_empty() {
            return Ok(Vec::new());
        }
        let labels_ref: Vec<&str> = self.labels.iter().map(|s| s.as_str()).collect();
        let texts = vec![processed.as_str()];
        let input = TextInput::from_str(&texts, &labels_ref)
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
        let mut seen: std::collections::HashMap<(String, String), usize> =
            std::collections::HashMap::new();
        for spans in output.spans {
            for span in spans {
                let prob = span.probability();
                if prob < self.threshold {
                    continue;
                }
                let text = normalize_mention_text(span.text());
                if text.is_empty() {
                    continue;
                }
                let label = span.class().to_string();
                let key = (text.to_lowercase(), label.clone());
                let (char_start, char_end) = span.offsets();
                let mention = EntityMention {
                    text,
                    label,
                    char_start,
                    char_end,
                    score: prob,
                };
                match seen.get(&key) {
                    Some(&idx) if mentions[idx].score >= mention.score => continue,
                    Some(&idx) => mentions[idx] = mention,
                    None => {
                        seen.insert(key, mentions.len());
                        mentions.push(mention);
                    }
                }
            }
        }
        Ok(mentions)
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
            let mut seen: std::collections::HashMap<(String, String), usize> =
                std::collections::HashMap::new();
            for span in spans {
                let prob = span.probability();
                if prob < self.threshold {
                    continue;
                }
                let text = normalize_mention_text(span.text());
                if text.is_empty() {
                    continue;
                }
                let label = span.class().to_string();
                let key = (text.to_lowercase(), label.clone());
                let (char_start, char_end) = span.offsets();
                let mention = EntityMention {
                    text,
                    label,
                    char_start,
                    char_end,
                    score: prob,
                };
                match seen.get(&key) {
                    Some(&idx) if chunk_mentions[idx].score >= mention.score => continue,
                    Some(&idx) => chunk_mentions[idx] = mention,
                    None => {
                        seen.insert(key, chunk_mentions.len());
                        chunk_mentions.push(mention);
                    }
                }
            }
            out[i] = chunk_mentions;
        }
        Ok(out)
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
        let Ok(mentions) = self.extract(text) else {
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
}

/// Collapse internal whitespace + trim. GliNER's span text
/// occasionally crosses newlines in the source (e.g. multi-line
/// "Jonathan\nSwift") because the model operates on the chunk
/// character stream including the chat-message internal line
/// breaks. Stored entity strings should be human-clean.
fn normalize_mention_text(raw: &str) -> String {
    raw.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Current unix timestamp — convenience for stamping
/// `extracted_at` across a batch of rows so consumers can see "this
/// whole corpus was extracted at T".
pub fn now_unix() -> i64 {
    Utc::now().timestamp()
}

/// Probe-style helper: returns true if the configured model is
/// installed and the extractor can be loaded. Useful for CLI
/// preflight before kicking off a long extraction.
pub fn probe_model_available(model_id: &str) -> bool {
    resolve_model_paths(model_id).is_ok()
}

/// Download model files for a given GliNER model id from
/// `huggingface.co/onnx-community/<model_id>`. Writes into
/// `models_root().join(model_id)`. Skip files already present at
/// the expected size — idempotent.
///
/// Reports progress via the `on_progress` callback (bytes downloaded,
/// total bytes). The desktop wires this to a status pill; the CLI
/// renders a percentage line. Long-running — typical model is
/// ~600MB.
pub async fn download_model(
    model_id: &str,
    on_progress: impl Fn(&str, u64, u64) + Send + Sync,
) -> Result<()> {
    let root = models_root().join(model_id);
    std::fs::create_dir_all(root.join("onnx"))
        .map_err(|e| Error::Storage(format!("create_dir_all {}: {e}", root.display())))?;

    let files = [
        ("tokenizer.json", root.join("tokenizer.json")),
        ("onnx/model.onnx", root.join("onnx").join("model.onnx")),
    ];

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
        let url =
            format!("https://huggingface.co/onnx-community/{model_id}/resolve/main/{remote_rel}");
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
}
