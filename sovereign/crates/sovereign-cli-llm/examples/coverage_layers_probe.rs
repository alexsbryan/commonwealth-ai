// SPDX-License-Identifier: AGPL-3.0-or-later
//! The coverage probe's calibration instrument: what does each VECTOR LAYER of
//! one corpus say about a question's coverage?
//!
//! `runtime/epistemic.rs::coverage_near_sim` carries a floor derived from a
//! measured in-topic/off-topic band and says, in its own doc, that the number
//! is global while the evidence is not — re-run the calibration on a second
//! corpus before treating it as settled. This is what re-runs it: feed it a
//! bank's questions and read the two bands off the `chunk` column.
//!
//! It also answers the question that killed a planned fix. The probe reads
//! `chunks.lance` and nothing else, and the obvious repair was to let it see
//! `raptor_summaries.lance` and the atlas seed table too. Measured over all 43
//! `secret_agent` questions on 2026-09-09, `max(chunk, raptor, atoms)` crosses
//! the floor for ZERO questions that `chunk` alone did not — a fix with no
//! failing input (ARCH §18.1). The three columns are printed side by side so
//! that stays checkable rather than remembered.
//!
//! `PROBE_SHOW_ATOMS=1` additionally prints the top-12 atlas atoms by cosine,
//! which is how you tell "the atlas has no vector for this atom" from "it has
//! one and the walk did not select it".
//!
//! ```text
//! cargo run -p sovereign-cli-llm --features corpus-engine/treesitter \
//!   --example coverage_layers_probe -- chaos-secret-agent "With what kind of weapon does Winnie kill Adolf Verloc?"
//! ```

use std::path::Path;
use std::sync::Arc;

use sovereign_core::traits::InferenceProvider;
use sovereign_inference::remote::SplitInferenceProvider;

fn cosine(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    let mut dot = 0.0f32;
    let mut na = 0.0f32;
    let mut nb = 0.0f32;
    for i in 0..a.len() {
        dot += a[i] * b[i];
        na += a[i] * a[i];
        nb += b[i] * b[i];
    }
    if na <= 0.0 || nb <= 0.0 {
        return 0.0;
    }
    dot / (na.sqrt() * nb.sqrt())
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_writer(std::io::stderr)
        .try_init();
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    if args.len() < 2 {
        eprintln!("usage: coverage_layers_probe <corpus_id> <question> [more questions...]");
        std::process::exit(2);
    }
    let corpus_id = args.remove(0);

    let daemon = std::env::var("SOVEREIGN_DAEMON_BASE")
        .unwrap_or_else(|_| "http://127.0.0.1:9741".to_string());
    let embed_model =
        std::env::var("SOVEREIGN_EMBED_MODEL").unwrap_or_else(|_| "qwen-embedding-0.6b".into());
    let v1 = format!("{daemon}/v1");
    let inference: Arc<dyn InferenceProvider> = Arc::new(SplitInferenceProvider::new(
        &v1,
        embed_model.clone(),
        embed_model.clone(),
        8192,
        sovereign_core::models_manifest::DEFAULT_MANIFEST.embed_query_instruction(&embed_model),
    ));

    let dotsov = sovereign_contracts::rebrand::svrnmesh_root();
    let embed_fn = sovereign_tools::corpus::inference_to_embed_fn(Arc::clone(&inference));
    let engine = Arc::new(
        corpus_engine::CorpusEngine::new(dotsov.join("recipes"), dotsov.join("indexes"), embed_fn)
            .with_embedding_model(&embed_model),
    );
    let corpus_dir = dotsov.join("indexes").join(&corpus_id);
    let atlas_dir = corpus_dir.join("atlas");

    println!("corpus={corpus_id} dir={}", corpus_dir.display());
    println!(
        "  chunks.lance={} raptor_summaries.lance={} atlas/atoms_ann.lance={}",
        corpus_dir.join("chunks.lance").exists(),
        corpus_dir.join("raptor_summaries.lance").exists(),
        atlas_dir.join("atoms_ann.lance").exists(),
    );
    println!("{:<8} {:<8} {:<8}  question", "chunk", "raptor", "atoms");

    for q in &args {
        let embedding = match inference.embed_query(q).await {
            Ok(e) => e,
            Err(e) => {
                eprintln!("embed failed ({e}) — is the daemon up with an embed model?");
                std::process::exit(1);
            }
        };

        // Layer 1: leaf chunks — exactly what coverage_probe reads today.
        let chunk_sim = match engine.open_index(&corpus_dir).await {
            Ok(idx) => match idx.nearest_vector_distance(&embedding, 8).await {
                Ok(Some(d)) => Some(1.0 - d),
                Ok(None) => None,
                Err(e) => {
                    eprintln!("  chunk probe error: {e}");
                    None
                }
            },
            Err(e) => {
                eprintln!("  open_index failed: {e}");
                None
            }
        };

        // Layer 2: RAPTOR summaries — exact cosine, already comparable.
        let raptor_sim =
            match corpus_engine::search_raptor_summaries(&corpus_dir, &embedding, 8).await {
                Ok(hits) => hits
                    .iter()
                    .map(|h| h.score)
                    .fold(None, |acc: Option<f32>, s| {
                        Some(acc.map_or(s, |a| a.max(s)))
                    }),
                Err(e) => {
                    eprintln!("  raptor probe error: {e}");
                    None
                }
            };

        // Layer 3: atlas atom seeds.
        let atoms_sim = probe_atoms(&atlas_dir, &embedding).await;

        let f = |v: Option<f32>| v.map(|x| format!("{x:.4}")).unwrap_or_else(|| "-".into());
        println!(
            "{:<8} {:<8} {:<8}  {}",
            f(chunk_sim),
            f(raptor_sim),
            f(atoms_sim),
            q
        );
    }
}

async fn probe_atoms(atlas_dir: &Path, embedding: &[f32]) -> Option<f32> {
    use corpus_engine::enrichment::atlas::ann_store::AnnSeedTable;
    let table = match AnnSeedTable::open_for_atlas(atlas_dir).await {
        Ok(t) => t,
        Err(e) => {
            eprintln!("  atoms open error: {e}");
            return None;
        }
    };
    match table.nearest_with_vectors(embedding, 12).await {
        Ok(rows) => {
            let mut scored: Vec<(String, f32)> = rows
                .iter()
                .map(|(k, v)| (k.clone(), cosine(embedding, v)))
                .collect();
            scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
            if std::env::var("PROBE_SHOW_ATOMS").is_ok() {
                for (k, s) in scored.iter().take(12) {
                    eprintln!("    atom {k:40} {s:.4}");
                }
            }
            scored.first().map(|(_, s)| *s)
        }
        Err(e) => {
            eprintln!("  atoms probe error: {e}");
            None
        }
    }
}
