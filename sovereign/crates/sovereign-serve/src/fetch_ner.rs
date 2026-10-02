// SPDX-License-Identifier: AGPL-3.0-or-later
//! `sovereign-serve fetch-ner`, spelled `svrn mesh fetch-ner` through the
//! dispatcher: fetch the NER kind's model, which serve loads
//! (`sovereign_compute::ner`). Moved from `svrn corpus extract-entities
//! --download-model` (pb-cli-llm): cli-llm loads no model, so fetching one
//! is serve's, beside `fetch-model` and `warm-cache`.

use sovereign_compute::ner::{configured_model_id, download_model, models_root};

/// `svrn mesh fetch-ner [<model_id>]`: the GLiNER ONNX and tokenizer from
/// huggingface.co/onnx-community/<model_id>, default the id serve loads
/// (`SOVEREIGN_GLINER_MODEL_ID`, else the shipped default). Idempotent: a
/// file already present is skipped. Reports per-file progress.
pub async fn cmd_fetch_ner(args: &[String]) -> i32 {
    const USAGE: &str = "Usage: svrn mesh fetch-ner [<model_id>]\n\n  \
        Fetches the NER (GLiNER) model serve loads from HuggingFace;\n  \
        default: SOVEREIGN_GLINER_MODEL_ID, else the shipped default.";
    let model_id = match args {
        [] => configured_model_id(),
        [flag] if flag == "--help" || flag == "-h" => {
            eprintln!("{USAGE}");
            return 0;
        }
        [id] if !id.starts_with('-') => id.clone(),
        _ => {
            eprintln!("{USAGE}");
            return 2;
        }
    };
    let root = models_root().join(&model_id);
    eprintln!("Downloading GliNER model '{model_id}' → {}", root.display());
    let last_pct = std::sync::Arc::new(std::sync::Mutex::new((String::new(), 0u8)));
    let progress_cb = {
        let last_pct = std::sync::Arc::clone(&last_pct);
        move |file: &str, downloaded: u64, total: u64| {
            if total == 0 {
                if downloaded == 0 {
                    eprintln!("  ✓ {file} already present");
                }
                return;
            }
            let pct = ((downloaded as f64 / total as f64) * 100.0) as u8;
            let mut lock = last_pct.lock().unwrap();
            if lock.0 != file || pct.saturating_sub(lock.1) >= 5 || pct == 100 {
                eprintln!(
                    "  {file}: {pct}% ({:.1} / {:.1} MB)",
                    downloaded as f64 / 1_048_576.0,
                    total as f64 / 1_048_576.0
                );
                lock.0 = file.to_string();
                lock.1 = pct;
            }
        }
    };
    match download_model(&model_id, progress_cb).await {
        Ok(()) => {
            eprintln!("✓ model installed at {}", root.display());
            eprintln!();
            eprintln!("  serve loads it at its next start; then: svrn corpus extract-entities <corpus_id>");
            0
        }
        Err(e) => {
            eprintln!("error: {e}");
            1
        }
    }
}
