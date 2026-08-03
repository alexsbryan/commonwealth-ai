// SPDX-License-Identifier: AGPL-3.0-or-later
//! SP1 spike probe (research/enrichment-spikes): can GLiNER2 ONNX graphs run
//! on the PINNED `ort =2.0.0-rc.9`, driven bare (no gline-rs), at ≥ v1
//! throughput — and does the export's schema contract accept a relation-style
//! field group?
//!
//! Three passes over the same fixture chunks, all timed after a warm call:
//!   1. v1 baseline: `GlinerExtractor::new_default()` (gline-rs stack,
//!      gliner_small-v2.1) via `extract_batch` — the apples-to-apples bar.
//!   2. GLiNER2 entities: bare `ort::Session` on a monolithic GLiNER2 export
//!      (lion-ai/gliner2-base-v1-onnx input contract), same label set.
//!   3. GLiNER2 relation trial: one structured field group
//!      `( [P] authorship ( [E] author [E] work title ) )` — whether slots
//!      fill through the same span-scoring head is part of SP1's answer.
//!
//! Run (fixture from research/enrichment-spikes/scripts/dump_chunks.py):
//!   cargo run --release -p sovereign-gliner --example gliner2_probe -- \
//!     <snapshot-dir-with-model.onnx-and-tokenizer.json> \
//!     research/enrichment-spikes/data/chunks_50.jsonl

use ort::session::Session;
use ort::value::Tensor;
use regex::Regex;
use std::time::Instant;
use tokenizers::Tokenizer;

const THRESHOLD: f32 = 0.5;
const MAX_WIDTH: usize = 8;
/// v1's DEFAULT_LABELS, lowercased per GLiNER2 preprocessing.
const ENTITY_LABELS: &[&str] = &["person", "organization", "work", "location", "event"];
/// Relation trial: one field group with two typed slots.
const RELATION_TASK: &str = "authorship";
const RELATION_SLOTS: &[&str] = &["author", "work title"];

struct Word {
    lower: String,
    start: usize,
    end: usize,
}

/// WhitespaceTokenSplitter regex from the export's README, run over the
/// ORIGINAL text (`(?i)` handles case) so char offsets stay valid even where
/// `to_lowercase()` would change byte lengths.
fn split_words(text: &str) -> Vec<Word> {
    let re = Regex::new(
        r"(?i)(?:https?://[^\s]+|www\.[^\s]+)|[a-z0-9._%+-]+@[a-z0-9.-]+\.[a-z]{2,}|@[a-z0-9_]+|\w+(?:[-_]\w+)*|\S",
    )
    .unwrap();
    re.find_iter(text)
        .map(|m| Word {
            lower: m.as_str().to_lowercase(),
            start: m.start(),
            end: m.end(),
        })
        .collect()
}

struct Hit {
    label: String,
    text: String,
    score: f32,
}

/// One GLiNER2 forward pass: schema = `( [P] <task> ( [E] f1 [E] f2 … ) )`.
fn gliner2_pass(
    session: &mut Session,
    tokenizer: &Tokenizer,
    task: &str,
    fields: &[&str],
    text: &str,
) -> Result<Vec<Hit>, Box<dyn std::error::Error>> {
    let words = split_words(text);
    let num_words = words.len();
    if num_words == 0 {
        return Ok(vec![]);
    }

    let mut schema_tokens: Vec<String> = vec!["(".into(), "[P]".into()];
    schema_tokens.extend(task.split_whitespace().map(String::from));
    schema_tokens.push("(".into());
    for f in fields {
        schema_tokens.push("[E]".into());
        schema_tokens.extend(f.split_whitespace().map(String::from));
    }
    schema_tokens.push(")".into());
    schema_tokens.push(")".into());
    let num_schema_words = schema_tokens.len() + 1; // +1 for [SEP_TEXT]

    let mut full: Vec<&str> = schema_tokens.iter().map(|s| s.as_str()).collect();
    full.push("[SEP_TEXT]");
    for w in &words {
        full.push(w.lower.as_str());
    }

    let encoding = tokenizer
        .encode(full, false)
        .map_err(|e| format!("tokenize: {e}"))?;
    let token_ids: Vec<i64> = encoding.get_ids().iter().map(|&id| id as i64).collect();
    let word_ids = encoding.get_word_ids();
    let seq_len = token_ids.len();

    // First-token position of each text word / schema marker.
    let first_tok = |target: u32| word_ids.iter().position(|&w| w == Some(target));
    let mut text_positions = Vec::with_capacity(num_words);
    for wi in 0..num_words {
        let pos = first_tok((num_schema_words + wi) as u32)
            .ok_or_else(|| format!("word {wi} missing from token mapping"))?;
        text_positions.push(pos as i64);
    }
    let mut schema_positions = Vec::new();
    for (i, tok) in schema_tokens.iter().enumerate() {
        if tok == "[P]" || tok == "[E]" {
            let pos =
                first_tok(i as u32).ok_or_else(|| format!("schema token {i} not mapped"))?;
            schema_positions.push(pos as i64);
        }
    }
    let num_schema_pos = schema_positions.len();

    let mut spans = Vec::with_capacity(num_words * MAX_WIDTH * 2);
    for start in 0..num_words {
        for width in 1..=MAX_WIDTH {
            if start + width <= num_words {
                spans.push(start as i64);
                spans.push((start + width - 1) as i64);
            } else {
                spans.push(0);
                spans.push(0);
            }
        }
    }

    let outputs = session.run(
        ort::inputs![
            "input_ids" => Tensor::from_array((vec![1i64, seq_len as i64], token_ids))?,
            "attention_mask" => Tensor::from_array((vec![1i64, seq_len as i64], vec![1i64; seq_len]))?,
            "text_positions" => Tensor::from_array((vec![num_words as i64], text_positions))?,
            "schema_positions" => Tensor::from_array((vec![num_schema_pos as i64], schema_positions))?,
            "span_idx" => Tensor::from_array((vec![1i64, (num_words * MAX_WIDTH) as i64, 2i64], spans))?,
        ]?,
    )?;

    let view = outputs["span_scores"].try_extract_tensor::<f32>()?;
    let shape = view.shape().to_vec(); // (1, num_fields, num_words, max_width)
    let num_fields = shape[1];

    let mut hits = Vec::new();
    for fi in 0..num_fields {
        for start in 0..num_words {
            for w in 0..MAX_WIDTH {
                let score = view[[0, fi, start, w]];
                if score >= THRESHOLD {
                    let end = start + w;
                    if end >= num_words {
                        continue;
                    }
                    hits.push(Hit {
                        label: fields.get(fi).unwrap_or(&"?").to_string(),
                        text: text[words[start].start..words[end].end].to_string(),
                        score,
                    });
                }
            }
        }
    }
    Ok(hits)
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let argv: Vec<String> = std::env::args().skip(1).collect();

    // `--only=v1|g2|rel|all` (default `all`).
    //
    // Isolation is what makes max-RSS ATTRIBUTABLE. With every pass in one
    // process the peak is the union of a gline-rs session, a bare-ort
    // session and a second schema run over the same session — which is
    // nobody's production residency. SP1 reported ~6.7 GB "incremental"
    // by subtracting a v1-solo run from that union; the 2026-08-02 re-run
    // measured the union at 11.7-12.0 GB and the subtraction is not a
    // valid attribution either way. `--only=g2` is the number that gates
    // a default flip.
    let only = argv
        .iter()
        .find_map(|a| a.strip_prefix("--only=").map(str::to_string))
        .unwrap_or_else(|| "all".into());
    let (run_v1, run_g2, run_rel) = match only.as_str() {
        "all" => (true, true, true),
        "v1" => (true, false, false),
        "g2" => (false, true, false),
        "rel" => (false, true, true),
        other => {
            // Refuse rather than silently falling back to `all`: a typo'd
            // mode that quietly ran every pass would report a union RSS
            // under the name of an isolated one.
            return Err(format!(
                "--only={other}: expected one of v1|g2|rel|all"
            )
            .into());
        }
    };

    let positional: Vec<&String> = argv.iter().filter(|a| !a.starts_with("--")).collect();
    let model_dir = positional
        .first()
        .map(|s| s.to_string())
        .unwrap_or_else(|| ".".into());
    let fixture = positional
        .get(1)
        .map(|s| s.to_string())
        .unwrap_or_else(|| "research/enrichment-spikes/data/chunks_50.jsonl".into());
    eprintln!("passes: v1={run_v1} g2={run_g2} rel={run_rel} (--only={only})");

    let raw = std::fs::read_to_string(&fixture)?;
    let chunks: Vec<String> = raw
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| {
            let v: serde_json::Value = serde_json::from_str(l).unwrap();
            v["text"].as_str().unwrap_or_default().to_string()
        })
        .collect();
    let total_words: usize = chunks.iter().map(|c| split_words(c).len()).sum();
    let total_chars: usize = chunks.iter().map(|c| c.len()).sum();
    eprintln!(
        "fixture: {} chunks, {total_words} words, {total_chars} chars\n",
        chunks.len()
    );

    // ── Pass 1: v1 baseline (gline-rs stack) ──
    if run_v1 {
        use sovereign_gliner::gliner_ner::GlinerExtractor;
        eprintln!("[v1] loading gliner_small-v2.1 (gline-rs)…");
        let t = Instant::now();
        let v1 = GlinerExtractor::new_default()?;
        eprintln!("[v1] loaded in {:.2?}", t.elapsed());
        let refs: Vec<&str> = chunks.iter().map(|s| s.as_str()).collect();
        let _ = v1.extract_batch(&refs[..2])?; // warm
        let t = Instant::now();
        let all = v1.extract_batch(&refs)?;
        let el = t.elapsed().as_secs_f64();
        let mentions: usize = all.iter().map(|m| m.len()).sum();
        println!("v1_total_s        {el:.2}");
        println!("v1_words_per_s    {:.0}", total_words as f64 / el);
        println!("v1_chunks_per_s   {:.2}", chunks.len() as f64 / el);
        println!("v1_mentions_per_chunk {:.1}", mentions as f64 / chunks.len() as f64);
        for (ci, ms) in all.iter().enumerate().take(3) {
            let sample: Vec<String> =
                ms.iter().take(6).map(|m| format!("{}:{}", m.label, m.text)).collect();
            eprintln!("  [v1] chunk {ci}: {sample:?}");
        }
    }

    // Everything below needs the GLiNER2 session; an early return keeps
    // `--only=v1` from paying its 795 MB load and polluting the RSS peak.
    if !run_g2 {
        return Ok(());
    }

    // ── Pass 2: GLiNER2 entities, bare ort rc.9 ──
    let tokenizer = Tokenizer::from_file(format!("{model_dir}/tokenizer.json"))
        .map_err(|e| format!("tokenizer: {e}"))?;
    eprintln!("\n[g2] loading {model_dir}/model.onnx on ort =2.0.0-rc.9…");
    let t = Instant::now();
    let mut session = Session::builder()?.commit_from_file(format!("{model_dir}/model.onnx"))?;
    eprintln!("[g2] loaded in {:.2?}", t.elapsed());

    let _ = gliner2_pass(&mut session, &tokenizer, "entities", ENTITY_LABELS, &chunks[0])?; // warm
    let t = Instant::now();
    let mut mentions = 0usize;
    let mut samples: Vec<String> = Vec::new();
    for (ci, c) in chunks.iter().enumerate() {
        let hits = gliner2_pass(&mut session, &tokenizer, "entities", ENTITY_LABELS, c)?;
        mentions += hits.len();
        if ci < 3 {
            samples.push(format!(
                "  [g2] chunk {ci}: {:?}",
                hits.iter()
                    .take(6)
                    .map(|h| format!("{}:{} ({:.2})", h.label, h.text, h.score))
                    .collect::<Vec<_>>()
            ));
        }
    }
    let el = t.elapsed().as_secs_f64();
    println!("g2_total_s        {el:.2}");
    println!("g2_words_per_s    {:.0}", total_words as f64 / el);
    println!("g2_chunks_per_s   {:.2}", chunks.len() as f64 / el);
    println!("g2_mentions_per_chunk {:.1}", mentions as f64 / chunks.len() as f64);
    for s in &samples {
        eprintln!("{s}");
    }

    if !run_rel {
        return Ok(());
    }

    // ── Pass 3: relation-style field group ──
    eprintln!("\n[g2-rel] schema ( [P] {RELATION_TASK} ( [E] {} ) )", RELATION_SLOTS.join(" [E] "));
    let t = Instant::now();
    let mut rel_hits = 0usize;
    for (ci, c) in chunks.iter().enumerate() {
        let hits = gliner2_pass(&mut session, &tokenizer, RELATION_TASK, RELATION_SLOTS, c)?;
        rel_hits += hits.len();
        if ci < 10 && !hits.is_empty() {
            eprintln!(
                "  [g2-rel] chunk {ci}: {:?}",
                hits.iter()
                    .take(4)
                    .map(|h| format!("{}:{} ({:.2})", h.label, h.text, h.score))
                    .collect::<Vec<_>>()
            );
        }
    }
    let el = t.elapsed().as_secs_f64();
    println!("g2rel_total_s     {el:.2}");
    println!("g2rel_slot_fills  {rel_hits}");

    Ok(())
}
