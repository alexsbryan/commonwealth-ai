// SPDX-License-Identifier: AGPL-3.0-or-later
//! Epistemic State I1 — live before/after demo (EPISTEMIC_STATE.md).
//!
//! Drives the REAL shipped machinery — the daemon's embed slot, the
//! real installed corpora, the real coverage probe
//! (`nearest_vector_distance` fan-out) and the real acquisition
//! resolver — on a question, and prints what the PREDECESSOR showed
//! the user next to what the epistemic ledger now knows. No mocks;
//! the only canned text is the predecessor's own abstention template,
//! quoted verbatim from `runtime/grounding/mod.rs::grounded_abstention`.
//!
//! Run (daemon must be up with an embed model loaded):
//!
//! ```text
//! cargo run -p sovereign-cli-llm --features corpus-engine/treesitter \
//!   --example epistemic_demo -- "Who did Leo Szilard write to in 1939?"
//! ```

use std::sync::Arc;

use sovereign_core::runtime::{acquisition, epistemic};
use sovereign_core::traits::InferenceProvider;
use sovereign_core::types::{AcquisitionRoute, GapCoverage};
use sovereign_inference::remote::SplitInferenceProvider;

const DEFAULT_QUESTIONS: &[&str] = &[
    // ClaimUncovered shape: the chaos corpus covers the novel, but
    // Conrad never wrote Heat's first name.
    "What is Chief Inspector Heat's first name in The Secret Agent?",
    // TopicUncovered shape: nothing installed is near this topic.
    "What were the key provisions of the EU AI Act's foundation-model rules?",
];

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
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

    let home = dirs::home_dir().expect("home dir");
    let dotsov = home.join(".sovereign");
    let embed_fn = sovereign_tools::corpus::inference_to_embed_fn(Arc::clone(&inference));
    let engine = Arc::new(
        corpus_engine::CorpusEngine::new(dotsov.join("recipes"), dotsov.join("indexes"), embed_fn)
            .with_embedding_model(&embed_model),
    );
    let n_installed = engine
        .installed_indexes()
        .await
        .map(|v| v.len())
        .unwrap_or(0);

    let questions: Vec<String> = if args.is_empty() {
        DEFAULT_QUESTIONS.iter().map(|s| s.to_string()).collect()
    } else {
        args
    };

    println!("epistemic-demo: daemon={daemon} embed={embed_model} installed_corpora={n_installed}");
    for q in &questions {
        demo_one(&inference, &engine, q).await;
    }
}

async fn demo_one(
    inference: &Arc<dyn InferenceProvider>,
    engine: &Arc<corpus_engine::CorpusEngine>,
    question: &str,
) {
    println!("\n════════════════════════════════════════════════════════════════");
    println!("QUESTION: {question}");

    // ── The predecessor's entire epistemic surface on a miss ──
    // Verbatim template from runtime/grounding/mod.rs::grounded_abstention
    // (still what ships when the ledger is disabled via
    // SOVEREIGN_EPISTEMIC_STATE=0).
    println!("\n── BEFORE (v0.3.0 abstention — the dead end) ──");
    println!(
        "  \"I couldn't confirm an answer to this against the passages your\n   \
         sources turned up — so rather than guess at something I can't verify\n   \
         from them, I'd flag that instead. If you think it's there, try\n   \
         rephrasing with the specific names or terms involved.\""
    );
    println!("  [no machine-readable verdict · no coverage signal · no next step]");

    // ── The I1 ledger's live signals ──
    let t = std::time::Instant::now();
    let embedding = match inference.embed_query(question).await {
        Ok(e) => e,
        Err(e) => {
            eprintln!("  embed failed ({e}) — is the daemon up with an embed model?");
            return;
        }
    };
    // `None` scope: probe all installed corpora (the demo has no sealed
    // notebook scope). Sealed turns pass the enabled corpora here.
    let probe = epistemic::coverage_probe(Some(engine), &embedding, None).await;
    println!("\n── AFTER (I1 epistemic ledger — live signals, this machine) ──");
    let coverage = match &probe {
        Some(p) => {
            println!(
                "  coverage probe: {:?}  (best nearest-chunk similarity {:.2}{}) [{}ms]",
                p.verdict,
                p.best_similarity,
                p.best_corpus
                    .as_deref()
                    .map(|c| format!(" in \"{c}\""))
                    .unwrap_or_default(),
                t.elapsed().as_millis(),
            );
            match p.verdict {
                GapCoverage::ClaimUncovered => println!(
                    "    → an installed corpus is NEAR this topic; the specific claim is what's missing"
                ),
                GapCoverage::TopicUncovered => println!(
                    "    → NO installed corpus has any region near this topic — acquiring a source is the remedy"
                ),
            }
            p.verdict
        }
        None => {
            println!("  coverage probe unavailable (engine/flag off)");
            GapCoverage::ClaimUncovered
        }
    };

    let gap_statement = format!(
        "Your sources didn't settle this question: {}",
        question.chars().take(160).collect::<String>()
    );
    let ctx = acquisition::RouteContext {
        engine: Some(Arc::clone(engine)),
        coverage: Some(coverage),
    };
    let t2 = std::time::Instant::now();
    // Resolution runs on the raw question (mirroring production's
    // demand-text resolution); the statement above is display-only.
    let routes = acquisition::routes_for_gap(inference.as_ref(), &ctx, question).await;
    println!(
        "  verdict: cannot_know_from_here · gap: \"{gap_statement}\"",
    );
    if routes.is_empty() {
        println!("  conjecture: (resolver disabled or nothing ranked)");
    } else {
        println!("  conjecture — where you could get this [{}ms]:", t2.elapsed().as_millis());
        for (i, r) in routes.iter().enumerate() {
            let label = match r {
                AcquisitionRoute::InstallRecipe { recipe_id, name } => {
                    format!("Install \"{name}\" ({recipe_id}) from the Library catalog")
                }
                AcquisitionRoute::ConnectFolder => "Connect a folder of your own documents".into(),
                AcquisitionRoute::ConnectVault => "Connect an Obsidian vault".into(),
                AcquisitionRoute::ImportConversations => {
                    "Import your assistant conversation exports".into()
                }
                AcquisitionRoute::WebSearch { queries } => {
                    format!("Search the web: {:?}", queries.first().map(|q| q.as_str()).unwrap_or(""))
                }
                AcquisitionRoute::ProvideDocument { kind } => format!("Provide: {kind}"),
            };
            println!("    {}. {label}", i + 1);
        }
    }
    println!(
        "  [typed EpistemicState persisted on the message · chaos 3rd lane scores this conjecture]"
    );
}
