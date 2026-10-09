// SPDX-License-Identifier: AGPL-3.0-or-later
//! The `LocalCorpusPort` double the local-corpus e2es drive (phase-b-47),
//! programmed over the leaf's own `CorpusIndex`. An ingest writes the staged
//! JSONL a svrn recipe points at into `<index_dir>/<corpus_id>`, one chunk
//! per row keyed by the row's id; a recipe with any other acquirer gets an
//! empty index; a watched update replaces a document's chunks with the text
//! svrn's fetch returns. That is as far as svrn's code reads back: the corpus
//! meta, chunk and doc ids, an index dir to hold a skeleton. What ingest
//! itself does with the same recipes is proven on `impl LocalCorpusPort for
//! CorpusEngine`, corpus-engine's tests/main/local_corpus_port_parity.rs.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::IngestPortDouble;
use crate::index::{CorpusIndex, InsertChunk};
use crate::ingest_port::{RecipeIngested, WatchedUpdate};
use crate::types::EmbedFn;
use crate::{Error, Result};

/// The double over `index_dir`, embedding with `embed`.
pub fn leaf_backed_double(index_dir: PathBuf, embed: EmbedFn) -> IngestPortDouble {
    let (ingest_dir, ingest_embed) = (index_dir.clone(), Arc::clone(&embed));
    let (empty_dir, empty_embed) = (index_dir.clone(), Arc::clone(&embed));
    let (update_dir, update_embed) = (index_dir.clone(), Arc::clone(&embed));
    let remove_dir = index_dir.clone();
    IngestPortDouble::new()
        .with_index_dir(index_dir)
        .opening_indexes_under_index_dir()
        .with_embed_fn(embed)
        .on_ingest_recipe_path(move |recipe| {
            let (dir, embed) = (ingest_dir.clone(), Arc::clone(&ingest_embed));
            Box::pin(async move { ingest(&dir, &embed, &recipe).await })
        })
        .on_ensure_empty_index(move |recipe| {
            let (dir, embed) = (empty_dir.clone(), Arc::clone(&empty_embed));
            Box::pin(async move {
                let head = RecipeHead::read(&recipe)?;
                open_or_create(&dir, &embed, &head).await.map(drop)
            })
        })
        .on_apply_watched_update(move |update, fetch| {
            let (dir, embed) = (update_dir.clone(), Arc::clone(&update_embed));
            Box::pin(async move { apply(&dir, &embed, &update, fetch).await })
        })
        .on_remove_corpus_everything(move |id| {
            let dir = remove_dir.join(id);
            if dir.exists() {
                std::fs::remove_dir_all(dir)?;
            }
            Ok(())
        })
        .on_ingest_in_flight(|_| false)
        .on_reindex_changed_sources_tiered(|_, _| {})
        .tearing_down_absent_atlases()
}

struct RecipeHead {
    corpus_id: String,
    name: String,
    /// The staged JSONL of a `local_file` acquirer.
    staged: Option<PathBuf>,
}

impl RecipeHead {
    fn read(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path)?;
        let doc: toml::Value = toml::from_str(&text).map_err(|e| Error::Recipe(e.to_string()))?;
        let field = |table: &str, key: &str| doc.get(table).and_then(|t| t.get(key));
        let required = |table: &str, key: &str| {
            field(table, key)
                .and_then(|v| v.as_str())
                .map(str::to_string)
                .ok_or_else(|| Error::Recipe(format!("recipe has no {table}.{key}")))
        };
        let corpus_id = required("corpus", "id")?;
        let name = required("corpus", "name")?;
        let staged = match field("acquire", "type").and_then(|v| v.as_str()) {
            Some("local_file") => field("acquire", "path")
                .and_then(|v| v.as_str())
                .map(PathBuf::from),
            _ => None,
        };
        Ok(Self {
            corpus_id,
            name,
            staged,
        })
    }
}

async fn open_or_create(dir: &Path, embed: &EmbedFn, head: &RecipeHead) -> Result<CorpusIndex> {
    let path = dir.join(&head.corpus_id);
    if crate::corpus::Corpus::meta_in(&path).is_file() {
        return CorpusIndex::open(&path).await;
    }
    let dim = embed("dimension probe").await?.len();
    CorpusIndex::create(
        &path,
        &head.corpus_id,
        &head.name,
        "test-mock",
        dim,
        false,
        "local",
    )
    .await
}

fn chunk(doc_id: &str, title: &str, content: &str) -> InsertChunk {
    InsertChunk {
        content: content.to_string(),
        title: Some(title.to_string()),
        url: None,
        metadata: None,
        content_hash: None,
        source_doc_id: Some(doc_id.to_string()),
        source_file: None,
        code: Default::default(),
        unit_id: None,
        text_sha256: None,
    }
}

async fn insert(index: &CorpusIndex, embed: &EmbedFn, chunks: Vec<InsertChunk>) -> Result<u64> {
    let mut batch = Vec::with_capacity(chunks.len());
    for c in chunks {
        let v = embed(&c.content).await?;
        batch.push((c, v));
    }
    index.insert_batch(&batch).await?;
    Ok(batch.len() as u64)
}

async fn ingest(dir: &Path, embed: &EmbedFn, recipe: &Path) -> Result<RecipeIngested> {
    let head = RecipeHead::read(recipe)?;
    let index = open_or_create(dir, embed, &head).await?;
    let mut chunks = Vec::new();
    if let Some(staged) = &head.staged {
        for line in std::fs::read_to_string(staged)?.lines() {
            let row: serde_json::Value =
                serde_json::from_str(line).map_err(|e| Error::Serialization(e.to_string()))?;
            let text = |k: &str| {
                row[k]
                    .as_str()
                    .ok_or_else(|| Error::Serialization(format!("staged row has no {k}: {row}")))
            };
            chunks.push(chunk(text("id")?, text("title")?, text("content")?));
        }
    }
    let chunks_created = insert(&index, embed, chunks).await?;
    Ok(RecipeIngested {
        corpus_id: head.corpus_id,
        chunks_created,
    })
}

async fn apply(
    dir: &Path,
    embed: &EmbedFn,
    update: &WatchedUpdate,
    fetch: crate::ingest_port::DocFetchFn,
) -> Result<()> {
    let index = CorpusIndex::open(&dir.join(&update.corpus_id)).await?;
    for doc in update
        .deleted_documents
        .iter()
        .chain(&update.updated_documents)
    {
        index.delete_chunks_by_source_doc(doc).await?;
    }
    let mut chunks = Vec::new();
    for doc in update.updated_documents.iter().chain(&update.new_documents) {
        chunks.push(chunk(doc, doc, &fetch(doc).await?.content));
    }
    insert(&index, embed, chunks).await.map(drop)
}
