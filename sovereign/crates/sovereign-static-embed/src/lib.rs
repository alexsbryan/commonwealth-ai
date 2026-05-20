//! Static text embeddings for the routing / scoping / intent layer.
//!
//! Distill a teacher embedding model (`EmbeddingGemma 300M` is the
//! first supported teacher) into a `vocab_size × dim` matrix of
//! Zipf-reweighted per-token embeddings. At runtime, embed a short
//! input by tokenising it through the same tokenizer the teacher
//! used, looking up the matrix row for each token, and mean-pooling
//! (with optional idf weights).
//!
//! Why static embeddings for routing-shaped calls:
//!
//! - The scope classifier, anaphoric-router pre-check, and atlas
//!   short-query sniff each pay one `embed_query` call per chat turn.
//!   The GPU embed slot is a `Mutex<SlotContext>` shared with chunk
//!   ingest; routing decisions serialise behind heavyweight encode
//!   batches.
//! - Inputs are typically <256 tokens with a small target vocabulary
//!   (scope centroids, intent classes). Per [Tulkens 2024], static
//!   embeddings retain ~92% of MTEB-quality vs the teacher on small-
//!   vocab nearest-neighbour tasks while paying zero GPU forward.
//! - Runtime cost: a `vocab.tokenize(text)` call + mean-pool over the
//!   resulting rows. ~0.1ms on warm caches vs ~50ms + slot contention
//!   for the GPU path.
//!
//! Crate-private invariants:
//! - `StaticEmbedder.matrix` is always `dim`-wide and matches
//!   `vocab.get_vocab_size()`. The artifact load path enforces this;
//!   downstream callers can rely on `dim()` returning the same value
//!   for the lifetime of the embedder.
//! - Mean-pool ignores special tokens (BOS / EOS / PAD). The token
//!   ids for these come from the tokenizer's `added_tokens` so the
//!   artifact stays portable across teachers that pick different ids.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{anyhow, Context, Result};
use ndarray::{Array1, Array2};
use safetensors::SafeTensors;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokenizers::Tokenizer;
use tracing::{debug, info, warn};

/// Trait every routing / scoping / intent callsite consults instead
/// of `InferenceProvider::embed_query` when a static-embed artifact is
/// configured. Deliberately separate from `InferenceProvider` — chunk
/// ingest stays on the qwen-embedding-0.6b GPU path; this trait is
/// scoped to short-input routing calls only.
///
/// `embed_normalized` is the common shape — all routing callers want
/// L2-normalised vectors for cosine math. `embed` exists for the rare
/// case where a caller does its own normalisation against a different
/// scheme.
pub trait ShortQueryEmbedder: Send + Sync {
    /// Return an unnormalised mean-pooled embedding of `text`.
    fn embed(&self, text: &str) -> Vec<f32>;

    /// Return an L2-normalised embedding (zero-vec on all-zero
    /// mean-pool, which can happen when every input token is a
    /// special token).
    fn embed_normalized(&self, text: &str) -> Vec<f32> {
        let mut v = self.embed(text);
        l2_normalize(&mut v);
        v
    }

    /// Dimensionality of the returned vectors. Constant for the
    /// embedder's lifetime.
    fn dim(&self) -> usize;

    /// Identifier of the teacher this embedder was distilled from
    /// (for example `"embeddinggemma-300M-BF16"`). Surfaced in
    /// `tracing` events so a glassbox operator can spot a mismatched
    /// artifact in production.
    fn teacher_id(&self) -> &str;
}

/// Artifact header serialised as JSON alongside the matrix tensor.
/// All numeric values are explicit (no defaults at deserialize time)
/// so a stale or hand-edited artifact fails loudly at load.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArtifactHeader {
    /// Free-form identifier of the teacher model. Convention: the
    /// gguf file stem, e.g. `"embeddinggemma-300M-BF16"`.
    pub teacher_id: String,
    /// Dimensionality of the per-token rows AFTER any MRL truncation
    /// the distillation tool applied. Runtime mean-pool produces
    /// `dim`-wide vectors directly.
    pub dim: usize,
    /// Vocabulary size — must match `tokenizer.get_vocab_size(true)`.
    /// Stored redundantly so a vocab/matrix mismatch surfaces at
    /// load rather than at first embed call.
    pub vocab_size: usize,
    /// Zipf reweighting alpha used at distill time, for telemetry.
    /// 0.0 = no reweight. Default 1.0 (matches Tulkens 2024 §3.2).
    pub zipf_alpha: f32,
    /// Whether the matrix was PCA-reduced from the teacher's native
    /// dimensionality. Informational; the matrix is the truth.
    pub pca_reduced: bool,
    /// ISO-8601 timestamp the artifact was written. Helps an operator
    /// match a stale artifact against a newer model on disk.
    pub created_at: String,
}

/// File names inside the artifact directory. The artifact is a
/// directory, not a single file, so a future addition (e.g. a
/// per-token idf vector) doesn't break the on-disk format.
pub mod artifact {
    pub const HEADER: &str = "header.json";
    pub const TOKENIZER: &str = "tokenizer.json";
    pub const MATRIX: &str = "matrix.safetensors";
    /// Optional — present iff distillation applied a per-token Zipf
    /// reweight that the runtime should multiply into mean-pool.
    /// Shape: `[vocab_size]`, f32.
    pub const IDF: &str = "idf.safetensors";
}

#[derive(Debug, Error)]
pub enum StaticEmbedError {
    #[error("artifact directory missing: {0}")]
    ArtifactMissing(PathBuf),
    #[error("artifact header malformed: {0}")]
    HeaderMalformed(String),
    #[error("vocab size {tokenizer} != header.vocab_size {header}")]
    VocabSizeMismatch { tokenizer: usize, header: usize },
    #[error("matrix shape {rows}x{cols} != header.vocab_size {vocab}, header.dim {dim}")]
    MatrixShapeMismatch {
        rows: usize,
        cols: usize,
        vocab: usize,
        dim: usize,
    },
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("safetensors: {0}")]
    SafeTensors(#[from] safetensors::SafeTensorError),
    #[error("tokenizer: {0}")]
    Tokenizer(String),
}

/// The runtime side of static embeddings — load a distilled artifact
/// and embed short inputs.
///
/// `Debug` is hand-rolled because `tokenizers::Tokenizer` doesn't
/// implement it. We surface the lightweight header fields + matrix
/// shape; the tokenizer is omitted (its inner state isn't useful in
/// a panic / `unwrap_err` diagnostic anyway).
pub struct StaticEmbedder {
    matrix: Array2<f32>,
    tokenizer: Tokenizer,
    idf: Option<Array1<f32>>,
    header: ArtifactHeader,
    /// Ids of tokenizer special tokens (BOS / EOS / PAD / UNK).
    /// Pulled from the tokenizer's added-tokens table at load time
    /// so mean-pool can skip them in O(1) per token.
    special_ids: Vec<u32>,
}

impl StaticEmbedder {
    /// Load a `.s2v` artifact directory.
    pub fn load(artifact_dir: impl AsRef<Path>) -> Result<Self, StaticEmbedError> {
        let dir = artifact_dir.as_ref();
        if !dir.exists() {
            return Err(StaticEmbedError::ArtifactMissing(dir.to_path_buf()));
        }
        let header_path = dir.join(artifact::HEADER);
        let header_bytes = std::fs::read(&header_path)?;
        let header: ArtifactHeader = serde_json::from_slice(&header_bytes)
            .map_err(|e| StaticEmbedError::HeaderMalformed(e.to_string()))?;

        let tokenizer_path = dir.join(artifact::TOKENIZER);
        let tokenizer = Tokenizer::from_file(&tokenizer_path)
            .map_err(|e| StaticEmbedError::Tokenizer(e.to_string()))?;
        // The HF tokenizer can advertise a larger vocab than the
        // teacher's `n_vocab` when its `added_tokens` table carries
        // ids beyond the model's embedding matrix (Gemma's tokenizer
        // ships with 262145 effective tokens but the model's
        // `n_vocab` is 262144 — one extra `<pad>`-class slot).
        // Treat tokenizer ≥ header as compatible; mean_pool already
        // gracefully skips ids that fall off the matrix.
        // Reject tokenizer < header — the matrix has rows the
        // tokenizer will never emit, signalling a stale tokenizer.
        let tokenizer_vocab = tokenizer.get_vocab_size(true);
        if tokenizer_vocab < header.vocab_size {
            return Err(StaticEmbedError::VocabSizeMismatch {
                tokenizer: tokenizer_vocab,
                header: header.vocab_size,
            });
        }

        let matrix_path = dir.join(artifact::MATRIX);
        let matrix_bytes = std::fs::read(&matrix_path)?;
        let matrix_st = SafeTensors::deserialize(&matrix_bytes)?;
        let matrix_view = matrix_st
            .tensor("matrix")
            .map_err(StaticEmbedError::SafeTensors)?;
        let shape = matrix_view.shape();
        if shape.len() != 2 || shape[0] != header.vocab_size || shape[1] != header.dim {
            return Err(StaticEmbedError::MatrixShapeMismatch {
                rows: shape.first().copied().unwrap_or(0),
                cols: shape.get(1).copied().unwrap_or(0),
                vocab: header.vocab_size,
                dim: header.dim,
            });
        }
        let matrix_data = matrix_view.data();
        // SAFETY: safetensors guarantees the data is naturally
        // aligned for the declared dtype. We declared f32; cast the
        // bytes through `bytemuck`-equivalent manual splat. Stay
        // unsafe-free by going through `f32::from_le_bytes` chunks —
        // distillation always writes little-endian.
        let n_floats = header.vocab_size * header.dim;
        let mut floats: Vec<f32> = Vec::with_capacity(n_floats);
        for chunk in matrix_data.chunks_exact(4) {
            floats.push(f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]));
        }
        let matrix = Array2::from_shape_vec((header.vocab_size, header.dim), floats)
            .map_err(|e| {
                StaticEmbedError::HeaderMalformed(format!("matrix reshape: {e}"))
            })?;

        let idf_path = dir.join(artifact::IDF);
        let idf = if idf_path.exists() {
            let idf_bytes = std::fs::read(&idf_path)?;
            let idf_st = SafeTensors::deserialize(&idf_bytes)?;
            let idf_view = idf_st.tensor("idf").map_err(StaticEmbedError::SafeTensors)?;
            let idf_floats: Vec<f32> = idf_view
                .data()
                .chunks_exact(4)
                .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
                .collect();
            if idf_floats.len() != header.vocab_size {
                warn!(
                    expected = header.vocab_size,
                    got = idf_floats.len(),
                    "static-embed: idf vector size mismatch — ignoring"
                );
                None
            } else {
                Some(Array1::from_vec(idf_floats))
            }
        } else {
            None
        };

        // Cache special-token ids for the fast skip path in
        // `mean_pool`. Tokenizer's added-tokens table is the
        // authoritative list — pull whatever it carries.
        let special_ids: Vec<u32> = tokenizer
            .get_added_tokens_decoder()
            .iter()
            .filter_map(|(id, tok)| if tok.special { Some(*id) } else { None })
            .collect();

        info!(
            teacher = %header.teacher_id,
            dim = header.dim,
            vocab = header.vocab_size,
            idf_present = idf.is_some(),
            specials = special_ids.len(),
            "static-embed: artifact loaded"
        );

        Ok(Self {
            matrix,
            tokenizer,
            idf,
            header,
            special_ids,
        })
    }

    fn mean_pool(&self, ids: &[u32]) -> Vec<f32> {
        let dim = self.header.dim;
        let mut acc = vec![0.0f32; dim];
        let mut weight_sum = 0.0f32;
        for &id in ids {
            if self.special_ids.contains(&id) {
                continue;
            }
            let row_idx = id as usize;
            if row_idx >= self.matrix.nrows() {
                // Unknown / out-of-range — skip rather than panic.
                continue;
            }
            let row = self.matrix.row(row_idx);
            let token_weight = self
                .idf
                .as_ref()
                .map(|idf| idf[row_idx])
                .unwrap_or(1.0);
            for (i, v) in row.iter().enumerate() {
                acc[i] += v * token_weight;
            }
            weight_sum += token_weight;
        }
        if weight_sum > 0.0 {
            for v in &mut acc {
                *v /= weight_sum;
            }
        }
        acc
    }
}

impl std::fmt::Debug for StaticEmbedder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StaticEmbedder")
            .field("teacher_id", &self.header.teacher_id)
            .field("dim", &self.header.dim)
            .field("vocab_size", &self.header.vocab_size)
            .field("matrix_shape", &(self.matrix.nrows(), self.matrix.ncols()))
            .field("has_idf", &self.idf.is_some())
            .field("special_ids", &self.special_ids.len())
            .finish()
    }
}

impl ShortQueryEmbedder for StaticEmbedder {
    fn embed(&self, text: &str) -> Vec<f32> {
        let encoding = match self.tokenizer.encode(text, false) {
            Ok(e) => e,
            Err(e) => {
                debug!(error = %e, "static-embed: tokenize failed — returning zero vector");
                return vec![0.0; self.header.dim];
            }
        };
        self.mean_pool(encoding.get_ids())
    }

    fn dim(&self) -> usize {
        self.header.dim
    }

    fn teacher_id(&self) -> &str {
        &self.header.teacher_id
    }
}

/// L2-normalise a vector in place. Returns the original (zero) vector
/// when the norm is below `f32::EPSILON` — calling code expects
/// `embed_normalized` to never NaN-out, and a zero vector is the
/// natural sentinel for "no signal" (e.g. an all-specials input).
fn l2_normalize(v: &mut [f32]) {
    let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > f32::EPSILON {
        for x in v {
            *x /= norm;
        }
    }
}

/// Convenience: wrap a loaded embedder in `Arc<dyn ShortQueryEmbedder>`
/// for stuffing into `Runtime.short_embed`.
pub fn load_default_artifact() -> Result<Option<Arc<dyn ShortQueryEmbedder>>> {
    let path = default_artifact_path()?;
    if !path.exists() {
        debug!(
            path = %path.display(),
            "static-embed: no artifact configured — routing falls through to GPU embed slot"
        );
        return Ok(None);
    }
    let embedder = StaticEmbedder::load(&path)
        .with_context(|| format!("loading static-embed artifact from {}", path.display()))?;
    Ok(Some(Arc::new(embedder)))
}

/// Resolve the canonical artifact path: `~/.sovereign/static-embed/active/`.
/// Returns `Err` if `$HOME` can't be resolved. The artifact directory
/// itself may not exist — callers check with `.exists()` before
/// loading.
pub fn default_artifact_path() -> Result<PathBuf> {
    let home = std::env::var_os("HOME")
        .ok_or_else(|| anyhow!("HOME not set; static-embed artifact path cannot be resolved"))?;
    Ok(PathBuf::from(home).join(".sovereign/static-embed/active"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use safetensors::tensor::{Dtype, TensorView};
    use std::collections::HashMap;

    /// Write a tiny fixture artifact directory with a deterministic
    /// 8-token vocab + 4-dim matrix. Mean-pool over known token ids
    /// gives a vector we can assert on without any GPU.
    fn write_fixture_artifact(dir: &std::path::Path) {
        std::fs::create_dir_all(dir).unwrap();
        // Use a minimal tokenizer.json with a wordlevel model so the
        // test doesn't need SentencePiece weights. 4 ascii tokens
        // map to ids 0..3.
        let tokenizer_json = serde_json::json!({
            "version": "1.0",
            "truncation": null,
            "padding": null,
            "added_tokens": [],
            "normalizer": null,
            "pre_tokenizer": {
                "type": "Whitespace"
            },
            "post_processor": null,
            "decoder": null,
            "model": {
                "type": "WordLevel",
                "vocab": {
                    "hello": 0,
                    "world": 1,
                    "static": 2,
                    "embed": 3
                },
                "unk_token": "[UNK]"
            }
        });
        std::fs::write(
            dir.join(artifact::TOKENIZER),
            serde_json::to_string_pretty(&tokenizer_json).unwrap(),
        )
        .unwrap();

        // 4-token vocab × 4-dim matrix. Row i = unit vector along
        // axis i so mean-pool over a known subset is predictable.
        // Each row pre-normalised so the matrix doubles as a
        // ground-truth axis set.
        let mut data: Vec<u8> = Vec::with_capacity(4 * 4 * 4);
        for row in 0..4 {
            for col in 0..4 {
                let v: f32 = if row == col { 1.0 } else { 0.0 };
                data.extend_from_slice(&v.to_le_bytes());
            }
        }

        let view = TensorView::new(Dtype::F32, vec![4, 4], &data).unwrap();
        let mut tensors: HashMap<String, TensorView> = HashMap::new();
        tensors.insert("matrix".to_string(), view);
        let bytes = safetensors::serialize(tensors, &None).unwrap();
        std::fs::write(dir.join(artifact::MATRIX), bytes).unwrap();

        let header = ArtifactHeader {
            teacher_id: "fixture".into(),
            dim: 4,
            vocab_size: 4,
            zipf_alpha: 0.0,
            pca_reduced: false,
            created_at: "2026-05-20T00:00:00Z".into(),
        };
        std::fs::write(
            dir.join(artifact::HEADER),
            serde_json::to_string_pretty(&header).unwrap(),
        )
        .unwrap();
    }

    #[test]
    fn loads_and_embeds_fixture() {
        let tmp = tempfile::tempdir().unwrap();
        write_fixture_artifact(tmp.path());
        let emb = StaticEmbedder::load(tmp.path()).expect("load fixture artifact");

        assert_eq!(emb.dim(), 4);
        assert_eq!(emb.teacher_id(), "fixture");

        // "hello world" → ids [0, 1] → mean of e0 + e1 = [0.5, 0.5, 0, 0].
        let v = emb.embed("hello world");
        assert!((v[0] - 0.5).abs() < 1e-6, "v[0] = {}", v[0]);
        assert!((v[1] - 0.5).abs() < 1e-6, "v[1] = {}", v[1]);
        assert!(v[2].abs() < 1e-6);
        assert!(v[3].abs() < 1e-6);
    }

    #[test]
    fn normalize_returns_unit_vector() {
        let tmp = tempfile::tempdir().unwrap();
        write_fixture_artifact(tmp.path());
        let emb = StaticEmbedder::load(tmp.path()).unwrap();

        let v = emb.embed_normalized("hello world");
        let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!((norm - 1.0).abs() < 1e-5, "norm = {norm}");
    }

    #[test]
    fn ranks_related_queries_higher() {
        // Sanity-check the cosine ordering: a query that overlaps
        // 100% on tokens should out-rank one that overlaps 50% which
        // should out-rank one that overlaps 0%. No GPU needed.
        let tmp = tempfile::tempdir().unwrap();
        write_fixture_artifact(tmp.path());
        let emb = StaticEmbedder::load(tmp.path()).unwrap();

        let anchor = emb.embed_normalized("hello world");
        let same = emb.embed_normalized("hello world");
        let half = emb.embed_normalized("hello static");
        let none = emb.embed_normalized("static embed");

        let cos = |a: &[f32], b: &[f32]| a.iter().zip(b).map(|(x, y)| x * y).sum::<f32>();
        let s_same = cos(&anchor, &same);
        let s_half = cos(&anchor, &half);
        let s_none = cos(&anchor, &none);

        assert!(s_same > s_half, "{s_same} > {s_half}");
        assert!(s_half > s_none, "{s_half} > {s_none}");
        // Same query → cosine ≈ 1.
        assert!((s_same - 1.0).abs() < 1e-5, "{s_same} != 1.0");
    }

    #[test]
    fn vocab_mismatch_errors_loudly() {
        let tmp = tempfile::tempdir().unwrap();
        write_fixture_artifact(tmp.path());
        // Corrupt the header to advertise a wrong vocab_size.
        let path = tmp.path().join(artifact::HEADER);
        let header: ArtifactHeader =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        let mut bad = header.clone();
        bad.vocab_size = 999;
        std::fs::write(&path, serde_json::to_string(&bad).unwrap()).unwrap();

        match StaticEmbedder::load(tmp.path()) {
            Err(StaticEmbedError::VocabSizeMismatch { .. }) => {}
            other => panic!("expected VocabSizeMismatch, got {other:?}"),
        }
    }
}
