// SPDX-License-Identifier: AGPL-3.0-or-later
//! `sovereign enrich sheets-ingest <folder> --corpus <id>` — the
//! described-asset substrate driver for spreadsheet corpora.
//!
//! Walks a folder of spreadsheets; for each, runs the XLSX sub-extractor
//! (calamine → noise-robust header detection → parquet parsed-form),
//! then the deterministic column-aware extractor (typed header →
//! Person/Organization entity); collects + re-ids the entities and
//! writes `<corpus>/atlas/atoms.json`. No LLM — column-aware is
//! structural — so this is the fast iteration loop for the attachment
//! substrate. Measure the output against a planted gold set with
//! `sovereign bench enron diagnose --corpus <id> --bench-dir <dir>`.
//!
//! This is the live wiring of `column_aware::extract_entities_from_parquet`
//! (previously built + unit-tested but with no pipeline caller).

use std::path::PathBuf;

use corpus_engine::asset_store::FilesystemAssetStore;
use corpus_engine::enrichment::atlas::atoms::{AtomEnvelope, AtomId, AtomsFile};
use corpus_engine::extractors::column_aware::{
    extract_entities_from_parquet, extract_entities_from_parquet_embed, ColumnAwareConfig,
    HeaderClassifier,
};
use corpus_engine::extractors::described_asset::AssetSubExtractor;
use corpus_engine::extractors::xlsx::XlsxSubExtractor;

use super::inference_client::DaemonInferenceClient;

pub async fn cmd_sheets_ingest(args: &[String]) -> i32 {
    let mut folder: Option<PathBuf> = None;
    let mut corpus: Option<String> = None;
    let mut indexes_dir: Option<PathBuf> = None;
    let mut keyword_mode = false;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--corpus" => corpus = it.next().cloned(),
            "--indexes-dir" => indexes_dir = it.next().map(PathBuf::from),
            "--keyword" => keyword_mode = true,
            "-h" | "--help" => {
                println!(
                    "usage: sovereign enrich sheets-ingest <folder> --corpus <id> \
                     [--indexes-dir <path>] [--keyword]\n\n\
                     Default classifies columns with the embed-centroid HeaderClassifier \
                     (semantic, generalizes); --keyword uses the substring header map \
                     (no daemon needed, in-sample only)."
                );
                return 0;
            }
            other if !other.starts_with('-') && folder.is_none() => {
                folder = Some(PathBuf::from(other));
            }
            other => {
                eprintln!("error: unexpected arg `{other}`");
                return 2;
            }
        }
    }
    let folder = match folder {
        Some(f) => f,
        None => {
            eprintln!("error: <folder> is required");
            return 2;
        }
    };
    let corpus = match corpus {
        Some(c) => c,
        None => {
            eprintln!("error: --corpus is required");
            return 2;
        }
    };
    let indexes_dir = indexes_dir.unwrap_or_else(default_indexes_dir);
    let atlas_dir = indexes_dir.join(&corpus).join("atlas");
    let assets_dir = indexes_dir.join(&corpus).join("assets");
    if let Err(e) = std::fs::create_dir_all(&atlas_dir) {
        eprintln!("error: create {}: {e}", atlas_dir.display());
        return 1;
    }

    let store = match FilesystemAssetStore::new(&assets_dir) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error: asset store at {}: {e}", assets_dir.display());
            return 1;
        }
    };
    let cfg = ColumnAwareConfig::default();

    // Default to the embed-centroid classifier (semantic, generalizes);
    // fall back to the keyword map on --keyword or if the daemon embed
    // endpoint is unreachable.
    let embed_classifier: Option<(HeaderClassifier, corpus_engine::types::EmbedFn)> =
        if keyword_mode {
            println!("  classifier: keyword map (--keyword)");
            None
        } else {
            match build_embed_classifier().await {
                Ok(pair) => {
                    println!("  classifier: embed-centroid (semantic)");
                    Some(pair)
                }
                Err(e) => {
                    eprintln!(
                        "  warn: embed classifier unavailable ({e}); falling back to keyword map"
                    );
                    None
                }
            }
        };

    let mut files: Vec<PathBuf> = match std::fs::read_dir(&folder) {
        Ok(rd) => rd
            .flatten()
            .map(|e| e.path())
            .filter(|p| {
                p.extension()
                    .and_then(|s| s.to_str())
                    .map(|s| s.eq_ignore_ascii_case("xlsx"))
                    .unwrap_or(false)
            })
            .collect(),
        Err(e) => {
            eprintln!("error: read_dir {}: {e}", folder.display());
            return 1;
        }
    };
    files.sort();
    if files.is_empty() {
        eprintln!("error: no .xlsx files in {}", folder.display());
        return 2;
    }

    println!("─── sheets-ingest: {} / {} files ───", corpus, files.len());
    let mut all_entities = Vec::new();
    let mut n_assets = 0usize;
    for path in &files {
        let bytes = match std::fs::read(path) {
            Ok(b) => b,
            Err(e) => {
                eprintln!("  skip {}: read error {e}", path.display());
                continue;
            }
        };
        let sha = sha256_hex(&bytes);
        let extraction = match XlsxSubExtractor.extract(path, &bytes, &sha, &store) {
            Ok(x) => x,
            Err(e) => {
                eprintln!("  skip {}: extract error {e}", path.display());
                continue;
            }
        };
        n_assets += 1;
        let fname = path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("asset.xlsx");
        match extraction.parsed_form.as_ref() {
            Some(parsed) => {
                let result = match embed_classifier.as_ref() {
                    Some((classifier, embed)) => {
                        extract_entities_from_parquet_embed(parsed, fname, classifier, embed, 0)
                            .await
                    }
                    None => extract_entities_from_parquet(parsed, fname, &cfg),
                };
                match result {
                    Ok(ents) => {
                        println!("  {fname:<30} → {} column-aware entities", ents.len());
                        all_entities.extend(ents);
                    }
                    Err(e) => eprintln!("  {fname}: column_aware error: {e}"),
                }
            }
            None => eprintln!("  {fname}: no tabular parsed_form"),
        }
    }

    // Globally re-id so per-file entity counters don't collide.
    let envelopes: Vec<AtomEnvelope> = all_entities
        .into_iter()
        .enumerate()
        .map(|(i, mut e)| {
            e.id = AtomId::entity(i + 1);
            AtomEnvelope::Entity(e)
        })
        .collect();
    let n_entities = envelopes.len();
    let atoms = AtomsFile::new(envelopes);
    let atoms_path = atlas_dir.join("atoms.json");
    let json = match serde_json::to_string_pretty(&atoms) {
        Ok(j) => j,
        Err(e) => {
            eprintln!("error: serialize atoms: {e}");
            return 1;
        }
    };
    if let Err(e) = std::fs::write(&atoms_path, json) {
        eprintln!("error: write {}: {e}", atoms_path.display());
        return 1;
    }
    println!(
        "wrote {n_entities} Entity atoms from {n_assets} assets → {}",
        atoms_path.display()
    );
    0
}

/// Build the embed-centroid header classifier against the local daemon's
/// `/v1/embeddings`. Returns the classifier + the EmbedFn (reused to
/// embed each column's signal during extraction).
async fn build_embed_classifier(
) -> std::result::Result<(HeaderClassifier, corpus_engine::types::EmbedFn), String> {
    let client = DaemonInferenceClient::new(
        "http://localhost:9741",
        "unused-chat-model",
        "qwen-embedding-0.6b",
    )
    .map_err(|e| format!("daemon client: {e}"))?;
    let (embed, _chat) = client.into_closures();
    let classifier = HeaderClassifier::build(&embed)
        .await
        .map_err(|e| format!("build centroids: {e}"))?;
    Ok((classifier, embed))
}

fn default_indexes_dir() -> PathBuf {
    std::env::var("HOME")
        .map(PathBuf::from)
        .unwrap_or_default()
        .join(".sovereign/indexes")
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(bytes);
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}
