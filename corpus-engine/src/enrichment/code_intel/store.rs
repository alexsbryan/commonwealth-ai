// SPDX-License-Identifier: AGPL-3.0-or-later
//! Storage: write per-symbol enrichments as searchable chunks (slice 3, Path B).
//!
//! Each [`SymbolEnrichment`] becomes one chunk in the corpus's existing
//! `chunks.lance`. Unlike RAPTOR (which keeps summaries in a separate table so
//! they do NOT surface in leaf retrieval), we *want* these summaries to surface
//! in normal retrieval — that IS the conceptual->symbol bridge (spec §3). The
//! chunk carries:
//!  - `content` = the rendered summary + questions (what retrieval matches),
//!  - the symbol's code metadata (name / file / lines) so a retrieval hit
//!    traces back to the symbol for call-graph navigation (Inc 2),
//!  - `source_doc_id` = a STABLE per-symbol key (survives body edits) — the
//!    upsert handle, namespaced so it never collides with raw chunks,
//!  - `content_hash` = the body hash — the delta gate: an unchanged body skips
//!    the re-embed + write entirely (the §3.4 cost model at the storage layer).
//!
//! Storage-agnostic generation (slices 1-2) feeds this; the choice to land on
//! the chunk index (vs. patchable Atlas atoms) is Path B of the §3.4 fork.

use crate::error::Result;
use crate::index::{CorpusIndex, InsertChunk, InsertCodeMeta};
use crate::types::EmbedFn;

use super::SymbolEnrichment;

/// Namespace prefix on `source_doc_id` so code-intel summary chunks never
/// collide with raw document/code chunks (whose `source_doc_id` is a file
/// path) and can be swept as a group.
const SOURCE_PREFIX: &str = "codeintel:";

/// Bump whenever [`render_for_index`] changes the text it produces for an
/// otherwise-unchanged symbol. v2 added the `Defined in <file>:<line>` anchor.
const RENDER_VERSION: u32 = 2;

/// The upsert identity stored as the summary chunk's `content_hash`.
///
/// It combines the symbol's body hash with the renderer version, because the
/// gate has to describe **the artifact we indexed**, not just its input. Keyed
/// on `body_hash` alone, an improvement to `render_for_index` was invisible:
/// every existing summary kept its old text forever, and no re-run could
/// dislodge it (the bodies hadn't changed, so every symbol "skipped"). The only
/// recovery was to hand-delete rows from LanceDB, which is not a thing an
/// operator should ever have to know. Folding the renderer version in makes the
/// upgrade automatic and exactly-once — and it stays cheap, because the summary
/// text itself is cached in `code_intel_cache.json` by body hash, so a
/// renderer bump re-embeds without re-running the model.
pub fn index_identity(e: &SymbolEnrichment) -> String {
    format!("{}/r{RENDER_VERSION}", e.body_hash)
}

/// Glassbox counts for one indexing pass (per-commit cost = `upserted`).
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct IndexReport {
    pub total: usize,
    /// New or changed body -> embedded + written.
    pub upserted: usize,
    /// Body unchanged, already indexed -> no embed, no write.
    pub skipped: usize,
    pub failed: usize,
}

/// Stable per-symbol key for the chunk's `source_doc_id` — the upsert handle.
/// Must survive a body edit (so the prior summary is found + replaced) and be
/// unique per symbol. Prefer the SCIP qualified name; fall back to `file#name`.
pub fn symbol_source_key(meta: &super::SymbolMeta) -> String {
    let id = if meta.qualified_name.is_empty() {
        format!("{}#{}", meta.file_path, meta.name)
    } else {
        meta.qualified_name.clone()
    };
    format!("{SOURCE_PREFIX}{id}")
}

/// The text that gets embedded + full-text-indexed: the user-voiced summary
/// followed by the questions it answers. Both are load-bearing — the scale run
/// showed `summary + asks` is the most robust retrieval signal (spec §5).
pub fn render_for_index(e: &SymbolEnrichment) -> String {
    let mut out = e.summary.clone();
    if !e.asks.is_empty() {
        out.push('\n');
        out.push_str(&e.asks.join("\n"));
    }
    // Anchor the symbol to its definition site IN THE INDEXED TEXT.
    //
    // The file path was only ever in the chunk's code metadata, which the
    // synthesis prompt never shows — so a model asked "where is X
    // implemented?" had no grounded path and invented one (observed: it
    // placed `gate_answer` in `src/citation.rs`; it lives in
    // `runtime/grounding/mod.rs`). Worse, the grounding gate had nothing to
    // check the claim against either, so the fabrication only surfaced as a
    // vague "could not be confirmed" note. One line of ground truth in the
    // body fixes both: the model cites the real path, and the gate can verify
    // it. Also a mild retrieval win for path-flavoured questions.
    if !e.meta.file_path.is_empty() {
        out.push_str(&format!(
            "\nDefined in {}:{}",
            e.meta.file_path, e.meta.line_start
        ));
    }
    out
}

fn insert_chunk_for(e: &SymbolEnrichment, key: &str, content: String) -> InsertChunk {
    let metadata = serde_json::json!({
        "source": "code_intel_summary",
        "symbol": e.meta.name,
        "qualified_name": e.meta.qualified_name,
    })
    .to_string();
    InsertChunk {
        content,
        title: Some(e.meta.name.clone()),
        url: None,
        metadata: Some(metadata),
        content_hash: Some(index_identity(e)),
        source_doc_id: Some(key.to_string()),
        source_file: None,
        // Carry the symbol identity so a retrieval hit on the summary traces
        // back to the symbol for call-graph navigation.
        code: InsertCodeMeta {
            symbol_name: Some(e.meta.name.clone()),
            symbol_kind: None,
            file_path: Some(e.meta.file_path.clone()),
            line_start: Some(e.meta.line_start as i32),
            line_end: Some(e.meta.line_end as i32),
            language: Some(e.meta.language.clone()),
            mtime: None,
        },
        unit_id: None,
    }
}

/// Returns `Ok(true)` if it wrote (new/changed), `Ok(false)` if it skipped
/// (body unchanged — its hash already sits under this symbol's key).
async fn index_one(index: &CorpusIndex, embed: &EmbedFn, e: &SymbolEnrichment) -> Result<bool> {
    let key = symbol_source_key(&e.meta);
    let committed = index.committed_chunks_for_doc(&key).await?;
    if committed
        .iter()
        .any(|c| c.content_hash == index_identity(e))
    {
        return Ok(false);
    }
    let content = render_for_index(e);
    let embedding = (embed)(&content).await?;
    index.delete_chunks_by_source_doc(&key).await?; // upsert: drop any prior summary
    index
        .insert_batch(&[(insert_chunk_for(e, &key, content), embedding)])
        .await?;
    Ok(true)
}

/// Upsert per-symbol enrichments into the corpus index, content-hash-gated.
/// For each enrichment: if a chunk for this symbol already carries the same
/// body-hash, skip it (no embed, no write); otherwise embed the rendered text,
/// delete any prior chunk for the symbol, and insert the fresh one. A single
/// failure is logged + counted, never aborting the batch.
pub async fn index_symbol_enrichments(
    index: &CorpusIndex,
    embed: &EmbedFn,
    enrichments: &[SymbolEnrichment],
) -> IndexReport {
    let mut report = IndexReport {
        total: enrichments.len(),
        ..Default::default()
    };
    for e in enrichments {
        match index_one(index, embed, e).await {
            Ok(true) => report.upserted += 1,
            Ok(false) => report.skipped += 1,
            Err(err) => {
                report.failed += 1;
                tracing::warn!(
                    target: "enrichment.code_intel",
                    symbol = %e.meta.name,
                    error = %err,
                    "indexing symbol enrichment failed; skipping",
                );
            }
        }
    }
    tracing::info!(
        target: "enrichment.code_intel",
        total = report.total,
        upserted = report.upserted,
        skipped = report.skipped,
        failed = report.failed,
        "code_intel: indexed symbol enrichments",
    );
    report
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::enrichment::code_intel::{SymbolEnrichment, SymbolMeta};
    use crate::index::CorpusIndex;
    use crate::types::EmbedFn;
    use std::sync::Arc;

    fn enr(name: &str, body_hash: &str, summary: &str) -> SymbolEnrichment {
        SymbolEnrichment {
            meta: SymbolMeta {
                name: name.to_string(),
                qualified_name: format!("crate::{name}"),
                file_path: "src/x.rs".to_string(),
                line_start: 1,
                line_end: 9,
                language: "rust".to_string(),
            },
            cache_key: String::new(),
            body_hash: body_hash.to_string(),
            summary: summary.to_string(),
            asks: vec!["What does it do?".to_string()],
        }
    }

    fn const_embed() -> EmbedFn {
        Arc::new(|_s: &str| Box::pin(async { Ok(vec![0.1_f32; 4]) }))
    }

    async fn empty_index(tag: &str) -> (tempfile::TempDir, CorpusIndex) {
        let dir = tempfile::tempdir().unwrap();
        let index = CorpusIndex::create(
            &dir.path().join(tag),
            tag,
            "Test",
            "test-model",
            4,
            false,
            "MIT",
        )
        .await
        .expect("create index");
        (dir, index)
    }

    #[test]
    fn source_key_is_namespaced_and_stable() {
        let e = enr("f", "h", "s");
        assert_eq!(symbol_source_key(&e.meta), "codeintel:crate::f");
        // A body edit (new hash) must NOT change the key — that's what makes the
        // upsert find + replace the old summary.
        let e2 = enr("f", "DIFFERENT", "s2");
        assert_eq!(symbol_source_key(&e.meta), symbol_source_key(&e2.meta));
    }

    #[test]
    fn render_joins_summary_and_asks() {
        let e = enr("f", "h", "It decides the route.");
        assert_eq!(
            render_for_index(&e),
            "It decides the route.\nWhat does it do?\nDefined in src/x.rs:1"
        );
    }

    /// The definition site must be IN THE BODY, not just the code metadata:
    /// the synthesis prompt shows the body only, so without it a "where is X
    /// implemented?" answer has no grounded path to cite and the gate has
    /// nothing to verify a path claim against.
    /// A row written before the renderer changed must NOT be treated as
    /// current. Keyed on `body_hash` alone it was — which is how an
    /// improvement to `render_for_index` could never reach an existing corpus.
    #[test]
    fn index_identity_is_renderer_versioned_not_just_the_body_hash() {
        let e = enr("f", "bodyhash", "s");
        let id = index_identity(&e);
        assert_ne!(id, e.body_hash, "identity must not be the bare body hash");
        assert!(id.starts_with(&e.body_hash), "body hash stays the prefix");
        // Same body, same renderer ⇒ same identity (the skip path still works).
        assert_eq!(index_identity(&enr("f", "bodyhash", "s")), id);
    }

    #[test]
    fn render_anchors_the_definition_site() {
        let e = enr("f", "h", "It decides the route.");
        assert!(render_for_index(&e).contains("Defined in src/x.rs:1"));

        // No path known (shouldn't happen via SCIP, but the renderer must not
        // emit a dangling "Defined in :0" that reads as ground truth).
        let mut bare = enr("g", "h", "Summary.");
        bare.meta.file_path = String::new();
        assert!(!render_for_index(&bare).contains("Defined in"));
    }

    #[tokio::test]
    async fn upsert_is_content_hash_gated_and_not_append() {
        let (_d, index) = empty_index("c").await;
        let embed = const_embed();

        // First pass: both new -> upserted, two rows.
        let rep = index_symbol_enrichments(
            &index,
            &embed,
            &[enr("f", "hAAA", "f one"), enr("g", "hBBB", "g one")],
        )
        .await;
        assert_eq!(rep.upserted, 2);
        assert_eq!(rep.skipped, 0);
        assert_eq!(index.chunk_count().await.unwrap(), 2);

        // Re-run unchanged -> both skipped (content-hash gate), still two rows.
        let rep2 = index_symbol_enrichments(
            &index,
            &embed,
            &[enr("f", "hAAA", "f one"), enr("g", "hBBB", "g one")],
        )
        .await;
        assert_eq!(rep2.skipped, 2);
        assert_eq!(rep2.upserted, 0);
        assert_eq!(
            index.chunk_count().await.unwrap(),
            2,
            "no new rows on a no-op"
        );

        // f's body changes (new hash) -> upsert f, skip g; still two rows (replace, not append).
        let rep3 = index_symbol_enrichments(
            &index,
            &embed,
            &[enr("f", "hCCC", "f two"), enr("g", "hBBB", "g one")],
        )
        .await;
        assert_eq!(rep3.upserted, 1, "f re-indexed");
        assert_eq!(rep3.skipped, 1, "g unchanged");
        assert_eq!(
            index.chunk_count().await.unwrap(),
            2,
            "upsert replaced f, did not append"
        );
    }
}
