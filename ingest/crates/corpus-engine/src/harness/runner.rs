// SPDX-License-Identifier: AGPL-3.0-or-later
//! The bounded staged runner. Reads the FROZEN sample, reconstructs the source,
//! and runs the REAL ingest stages (acquire-frozen → extract → filter → chunk),
//! emitting typed observations.
//!
//! I2 ("no second pipeline") holds because this goes through the exact same
//! `make_extractor` / `make_chunker` factories and the shared
//! [`crate::engine::chunk_doc`] the production ingest uses — and
//! `parity_runner_matches_inline` pins that: the runner's extract+chunk output
//! over the frozen, materialized source is byte-identical to running the same
//! factories inline over the original source.

use std::path::Path;

use crate::engine::{blake3_hex, chunk_doc, CorpusEngine};
use crate::error::Result;
use crate::filters::build_filter_pipeline;
use crate::index::{CorpusIndex, InsertChunk, InsertCodeMeta};
use crate::recipe::Recipe;
use crate::testing::chunker_max_chars;

use super::frozen_sample::FrozenSample;
use super::miss::FieldMiss;
use super::stage_output::{ChunkOutput, ExtractOutput, FilterOutput, IndexOutput, StageOutputs};

/// Runs the deterministic rungs (1–3) over a frozen sample.
pub struct HarnessRunner<'a> {
    engine: &'a CorpusEngine,
    recipe: &'a Recipe,
    frozen: &'a FrozenSample,
}

impl<'a> HarnessRunner<'a> {
    pub fn new(engine: &'a CorpusEngine, recipe: &'a Recipe, frozen: &'a FrozenSample) -> Self {
        Self {
            engine,
            recipe,
            frozen,
        }
    }

    /// Materialize the frozen source into `work_dir`, then run
    /// extract → filter → chunk bounded to `sample_size`, returning the
    /// per-stage observations. Synchronous and model-free — the network never
    /// runs (the bytes are frozen) and no embedding model is needed.
    pub async fn run(&self, work_dir: &Path, sample_size: usize) -> Result<StageOutputs> {
        std::fs::create_dir_all(work_dir)?;
        let source_path = self.frozen.materialize(work_dir)?;

        // ── Extract (same factory as ingest) ─────────────────────────────
        let extractor = self
            .engine
            .make_extractor(&self.recipe.extract, &self.recipe.corpus.id);
        let mut docs = Vec::new();
        let mut errors = Vec::new();
        let mut attempted = 0usize;
        for result in extractor.extract(&source_path)?.take(sample_size) {
            attempted += 1;
            match result {
                Ok(doc) => docs.push(doc),
                Err(e) => {
                    if errors.len() < 10 {
                        errors.push(e.to_string());
                    }
                }
            }
        }
        let extract = ExtractOutput {
            docs: docs.clone(),
            attempted,
            errors,
            source_files: self.frozen.manifest.source_files.len(),
            section_misses: slurp_section_misses(&source_path),
        };

        // ── Filter (same pipeline builder as ingest) ─────────────────────
        let pipeline = build_filter_pipeline(
            &self.recipe.filters,
            self.recipe.filter_mode.mode,
            Some(self.engine.recipes_dir()),
        )?;
        let active = pipeline.is_active();
        let descriptions = pipeline.descriptions();
        let (kept, dropped): (Vec<_>, Vec<_>) = docs
            .into_iter()
            .partition(|d| !active || pipeline.accept(d));
        let filter = FilterOutput {
            active,
            kept,
            dropped,
            descriptions,
        };

        // ── Chunk (same chunker + shared chunk_doc as ingest) ────────────
        let chunker = self.engine.make_chunker(&self.recipe.chunk);
        let mut chunks = Vec::new();
        let mut per_doc_counts = Vec::new();
        for doc in &filter.kept {
            let cs = chunk_doc(chunker.as_ref(), doc);
            per_doc_counts.push(cs.len());
            chunks.extend(cs);
        }
        let chunk = ChunkOutput {
            chunks,
            per_doc_counts,
            declared_max_chars: chunker_max_chars(&self.recipe.chunk),
        };

        // ── Index round-trip (FTS keyword path — model-free) ─────────────
        let index = self.index_roundtrip(&chunk.chunks, work_dir).await;

        Ok(StageOutputs {
            extract,
            filter,
            chunk,
            index,
        })
    }

    /// Build a tiny FTS index from the chunk texts and round-trip a
    /// deterministically-chosen rare token through it. Model-free: chunks are
    /// inserted with zero-vectors at a fixed dim and only the Tantivy FTS index
    /// is built (`build_indexes(false, true, …)`), so no embedding model runs.
    /// Errors are captured into the returned [`IndexOutput`] rather than
    /// propagated — a failed build is itself the verdict.
    async fn index_roundtrip(&self, chunks: &[String], work_dir: &Path) -> IndexOutput {
        let model = self.recipe.index.embedding_model.clone();
        let mut out = IndexOutput {
            built: false,
            model_declared: model.clone(),
            model_recorded: String::new(),
            token: None,
            source_preview: None,
            roundtrip_ok: false,
            hit_count: 0,
            error: None,
        };
        if chunks.is_empty() {
            out.error = Some("no chunks to index".into());
            return out;
        }
        let index_dir = work_dir.join("harness-index");
        let _ = std::fs::remove_dir_all(&index_dir);
        const DIM: usize = 8; // vectors are unused on the FTS path; keep it tiny
        let index = match CorpusIndex::create(
            &index_dir,
            &self.recipe.corpus.id,
            &self.recipe.corpus.name,
            &model,
            DIM,
            self.recipe.corpus.mesh_sharing,
            &self.recipe.corpus.license,
        )
        .await
        {
            Ok(i) => i,
            Err(e) => {
                out.error = Some(format!("create: {e}"));
                return out;
            }
        };
        let batch: Vec<(InsertChunk, Vec<f32>)> = chunks
            .iter()
            .enumerate()
            .map(|(i, c)| {
                (
                    InsertChunk {
                        content: c.clone(),
                        title: None,
                        url: None,
                        metadata: None,
                        content_hash: Some(blake3_hex(c)),
                        source_doc_id: Some(format!("chunk-{i}")),
                        source_file: None,
                        code: InsertCodeMeta::default(),
                        unit_id: None,
                    },
                    vec![0.0f32; DIM],
                )
            })
            .collect();
        if let Err(e) = index.insert_batch(&batch).await {
            out.error = Some(format!("insert: {e}"));
            return out;
        }
        if let Err(e) = index.build_indexes(false, true, None).await {
            out.error = Some(format!("build_indexes: {e}"));
            return out;
        }
        out.built = true;
        // We created the index with the declared model, so the recorded model
        // matches by construction on this model-free path. The *real*
        // model-match (engine embed model vs declared) lives on the `--enrich`
        // path, which builds a real-embeddings index.
        out.model_recorded = model;

        if let Some((token, source_idx)) = pick_rare_token(chunks) {
            let source = chunks[source_idx].clone();
            out.source_preview = Some(source.chars().take(120).collect());
            out.token = Some(token.clone());
            match index.search(&[], &token, 10).await {
                Ok(hits) => {
                    out.hit_count = hits.len();
                    out.roundtrip_ok = hits.iter().any(|h| h.content == source);
                }
                Err(e) => out.error = Some(format!("fts search: {e}")),
            }
        }
        out
    }
}

/// Pick a deterministic "rare" token from the chunk set: the alphanumeric word
/// (≥4 chars, containing a letter) that appears in the fewest chunks, breaking
/// ties by longest then lexicographically so the choice is stable regardless of
/// hash-map iteration order (I1). Returns the token and the index of the first
/// chunk it appears in.
fn pick_rare_token(chunks: &[String]) -> Option<(String, usize)> {
    use std::collections::{HashMap, HashSet};
    let mut occ: HashMap<String, Vec<usize>> = HashMap::new();
    for (i, c) in chunks.iter().enumerate() {
        let mut seen = HashSet::new();
        for raw in c.split(|ch: char| !ch.is_alphanumeric()) {
            if raw.len() >= 4 && raw.chars().any(char::is_alphabetic) {
                let t = raw.to_lowercase();
                if seen.insert(t.clone()) {
                    occ.entry(t).or_default().push(i);
                }
            }
        }
    }
    occ.into_iter()
        .min_by(|(at, ai), (bt, bi)| {
            ai.len()
                .cmp(&bi.len())
                .then_with(|| bt.len().cmp(&at.len()))
                .then_with(|| at.cmp(bt))
        })
        .map(|(token, idxs)| (token, idxs[0]))
}

/// Slurp the html_sections `_section_misses.json` sidecar if present, lifting
/// each entry into the generalized [`FieldMiss`]. Mirrors the lookup the legacy
/// `recipe test` does (the sidecar sits next to the source or one level up).
fn slurp_section_misses(source_path: &Path) -> Vec<FieldMiss> {
    let candidates = [
        source_path.join("_section_misses.json"),
        source_path
            .parent()
            .map(|p| p.join("_section_misses.json"))
            .unwrap_or_default(),
    ];
    for candidate in candidates {
        if candidate.is_file() {
            if let Ok(raw) = std::fs::read_to_string(&candidate) {
                if let Ok(parsed) =
                    serde_json::from_str::<Vec<crate::extractors::html_sections::MissReport>>(&raw)
                {
                    return parsed.into_iter().map(Into::into).collect();
                }
            }
        }
    }
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::harness::{capture, FrozenSample};

    fn stub_embed() -> crate::types::EmbedFn {
        std::sync::Arc::new(|_t: &str| Box::pin(async { Ok(vec![0.0_f32; 8]) }))
    }

    fn fixture_recipe(jsonl: &Path) -> Recipe {
        let toml = format!(
            r#"
[corpus]
id = "harness-parity"
name = "Harness Parity"

[acquire]
type = "local_file"
path = "{}"

[extract]
type = "jsonl"
content_field = "text"
title_field = "title"

[chunk]
type = "paragraph"
max_chars = 2048

[index]
embedding_model = "stub"
"#,
            jsonl.display()
        );
        Recipe::from_toml(&toml).expect("fixture recipe parses")
    }

    /// I2 guard: the runner's extract+chunk over the FROZEN, materialized
    /// source is identical to running the same factories inline over the
    /// ORIGINAL source. If anyone forks the orchestration later, this fails.
    #[tokio::test]
    async fn parity_runner_matches_inline() {
        let dir = tempfile::tempdir().unwrap();
        let jsonl = dir.path().join("fixture.jsonl");
        std::fs::write(
            &jsonl,
            "{\"title\":\"Alpha\",\"text\":\"alpha body one\"}\n\
             {\"title\":\"Bravo\",\"text\":\"bravo body two\"}\n\
             {\"title\":\"Charlie\",\"text\":\"charlie body three\"}\n",
        )
        .unwrap();

        let engine = CorpusEngine::new(
            dir.path().join("recipes"),
            dir.path().join("indexes"),
            stub_embed(),
        );
        let recipe = fixture_recipe(&jsonl);

        let harness_root = dir.path().join("h");
        capture(&engine, &recipe, &harness_root, 50).await.unwrap();
        let frozen = FrozenSample::load(&harness_root).unwrap().unwrap();

        let runner = HarnessRunner::new(&engine, &recipe, &frozen);
        let out = runner.run(&dir.path().join("work"), 50).await.unwrap();

        // Inline: the same factories, run directly over the original source.
        let extractor = engine.make_extractor(&recipe.extract, &recipe.corpus.id);
        let inline_docs: Vec<_> = extractor
            .extract(&jsonl)
            .unwrap()
            .take(50)
            .filter_map(std::result::Result::ok)
            .collect();
        let chunker = engine.make_chunker(&recipe.chunk);
        let inline_chunks: Vec<String> = inline_docs
            .iter()
            .flat_map(|d| chunk_doc(chunker.as_ref(), d))
            .collect();

        assert_eq!(
            out.extract.docs.len(),
            inline_docs.len(),
            "same doc count over frozen vs original source"
        );
        let runner_contents: Vec<&String> = out.extract.docs.iter().map(|d| &d.content).collect();
        let inline_contents: Vec<&String> = inline_docs.iter().map(|d| &d.content).collect();
        assert_eq!(
            runner_contents, inline_contents,
            "I2: extract over frozen sample == inline over original"
        );
        assert_eq!(
            out.chunk.chunks, inline_chunks,
            "I2: chunk_doc identical on both paths"
        );

        // No filters declared → everything kept, nothing dropped.
        assert!(!out.filter.active);
        assert_eq!(out.filter.kept.len(), 3);
        assert!(out.filter.dropped.is_empty());

        // Index rung built a model-free FTS index and round-tripped a token.
        assert!(out.index.built, "FTS index built");
        assert!(
            out.index.roundtrip_ok,
            "rare token should return its source chunk via FTS"
        );
    }
}
