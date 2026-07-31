// SPDX-License-Identifier: AGPL-3.0-or-later
//! SP5 spike probe (research/enrichment-spikes): noun-phrase extraction +
//! co-occurrence graph + Leiden communities over 10k wikipedia chunks —
//! adopt-or-write evidence for P2.2.
//!
//! Gate (README G5): < 5 min single-machine CPU for the full pipeline AND
//! 20 sampled communities eyeball-cohere against article titles.
//!
//! Method (POS-free, patterned on `extract_motif_candidates`'s
//! tokenization/stoplist/df machinery in sovereign-tools/document_asset.rs
//! — replicated here, not imported: spikes don't touch production crates):
//!   1. RAKE-style candidate phrases: token runs between stopwords (1-4
//!      tokens), plus capitalization runs — lowercased, df-counted.
//!   2. Vocabulary: df-band prune (3 <= df, df/N <= 0.3), tf-idf rank,
//!      top-V concepts.
//!   3. Edges: chunk-window co-occurrence counts, weight >= 2 kept.
//!   4. Communities: leiden-rs (survey pick) on the concept graph.
//!
//! Run:
//!   cargo run -p corpus-engine --features treesitter --example concept_graph_probe -- \
//!     research/enrichment-spikes/data/sp5_wiki_10k.jsonl \
//!     research/enrichment-spikes/runs/sp5/communities.txt

use leiden_rs::{GraphDataBuilder, Leiden, LeidenConfig};
use std::collections::{HashMap, HashSet};
use std::io::Write as _;
use std::time::Instant;

const TOP_V: usize = 5_000; // concept vocabulary size
const MAX_PHRASE_TOKENS: usize = 4;
const MIN_DF: usize = 3;
// 0.3 (the motif single-doc band) admitted corpus-generic vocabulary
// ("time", "years", "according") at 10k-chunk scale — those seeded a junk
// mega-community in run 1. 0.05 = df <= 500 chunks.
const MAX_DF_FRAC: f64 = 0.05;
const MIN_COOC: f64 = 2.0; // raw co-occurrence prefilter
const CHUNK_CONCEPT_CAP: usize = 40; // bound quadratic pair fan-out per chunk

// Compact English stopword list — the phrase-boundary set. Same role as
// MOTIF_STOPLIST (document_asset.rs), trimmed to boundary words.
const STOPWORDS: &[&str] = &[
    "a", "an", "the", "and", "or", "but", "nor", "so", "yet", "of", "in", "on", "at", "to",
    "from", "by", "with", "as", "for", "into", "onto", "upon", "about", "above", "below",
    "between", "among", "through", "during", "before", "after", "over", "under", "again",
    "further", "then", "once", "here", "there", "when", "where", "why", "how", "all", "any",
    "both", "each", "few", "more", "most", "other", "some", "such", "no", "not", "only",
    "own", "same", "than", "too", "very", "can", "will", "just", "should", "could", "would",
    "may", "might", "must", "shall", "is", "are", "was", "were", "be", "been", "being",
    "have", "has", "had", "having", "do", "does", "did", "doing", "it", "its", "itself",
    "this", "that", "these", "those", "i", "you", "he", "she", "they", "we", "his", "her",
    "their", "our", "your", "them", "him", "who", "whom", "which", "what", "also", "one",
    "two", "first", "second", "new", "many", "much", "well", "however", "although", "while",
    "because", "if", "since", "until", "unless", "whether", "either", "neither", "became",
    "become", "used", "known", "called", "made", "including", "within", "without",
    // Calendar terms: dates are corpus-generic in encyclopedic text, not
    // concepts — they seeded a biography/date mush community in run 2.
    "january", "february", "march", "april", "june", "july", "august", "september",
    "october", "november", "december", "monday", "tuesday", "wednesday", "thursday",
    "friday", "saturday", "sunday", "year", "years", "century", "day", "days", "time",
];

#[derive(serde::Deserialize)]
struct ChunkRow {
    #[allow(dead_code)]
    id: i64,
    title: String,
    content: String,
}

/// RAKE-style phrase candidates + capitalization runs, lowercased. Returns
/// the per-chunk multiset as (phrase, count).
fn extract_phrases(text: &str, stop: &HashSet<&str>) -> HashMap<String, u32> {
    let mut out: HashMap<String, u32> = HashMap::new();
    // Token stream per sentence-ish segment; boundaries at non-word chars
    // (keeping '-' and '\'' inside tokens).
    for line in text.split(['\n', '.', '!', '?', ';', ':']) {
        let tokens: Vec<&str> = line
            .split(|c: char| !c.is_alphanumeric() && c != '-' && c != '\'')
            .filter(|t| !t.is_empty())
            .collect();
        // RAKE segments: runs between stopwords.
        let mut seg: Vec<String> = Vec::new();
        let flush = |seg: &mut Vec<String>, out: &mut HashMap<String, u32>| {
            if !seg.is_empty() && seg.len() <= MAX_PHRASE_TOKENS {
                let phrase = seg.join(" ");
                if phrase.len() >= 4 && phrase.len() <= 60 {
                    *out.entry(phrase).or_insert(0) += 1;
                }
            }
            seg.clear();
        };
        for tok in &tokens {
            let lower = tok.to_lowercase();
            let bare = lower.trim_end_matches("'s").trim_end_matches('\'');
            let is_stop = stop.contains(bare)
                || bare.len() < 3
                || bare.chars().all(|c| c.is_ascii_digit());
            if is_stop {
                flush(&mut seg, &mut out);
            } else {
                seg.push(bare.to_string());
                if seg.len() > MAX_PHRASE_TOKENS {
                    flush(&mut seg, &mut out);
                }
            }
        }
        flush(&mut seg, &mut out);
        // Capitalization runs (catch NPs whose inner tokens are stopwords,
        // e.g. "Bank of England" — the RAKE pass splits those).
        let mut run: Vec<String> = Vec::new();
        for (i, tok) in tokens.iter().enumerate() {
            let capitalized = tok.chars().next().is_some_and(|c| c.is_uppercase());
            let lower = tok.to_lowercase();
            let bridges = matches!(lower.as_str(), "of" | "de" | "von" | "van" | "the" | "and");
            if capitalized && !(i == 0 && run.is_empty() && tokens.len() > 1) {
                run.push(lower);
            } else if bridges && !run.is_empty() {
                run.push(lower);
            } else {
                if run.last().is_some_and(|t| matches!(t.as_str(), "of" | "de" | "von" | "van" | "the" | "and")) {
                    run.pop();
                }
                if run.len() >= 2 && run.len() <= MAX_PHRASE_TOKENS {
                    let phrase = run.join(" ");
                    if phrase.len() <= 60 {
                        *out.entry(phrase).or_insert(0) += 1;
                    }
                }
                run.clear();
            }
        }
        if run.len() >= 2 && run.len() <= MAX_PHRASE_TOKENS {
            *out.entry(run.join(" ")).or_insert(0) += 1;
        }
    }
    out
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let chunks_path = args.next().expect("arg 1: chunks JSONL (sp5_dump_wiki.py output)");
    let out_path = args.next().expect("arg 2: communities report path");
    let resolution: f64 = args.next().map(|s| s.parse().expect("resolution")).unwrap_or(1.0);

    let t_total = Instant::now();
    let stop: HashSet<&str> = STOPWORDS.iter().copied().collect();

    let t = Instant::now();
    let raw = std::fs::read_to_string(&chunks_path)?;
    let chunks: Vec<ChunkRow> = raw
        .lines()
        .map(|l| serde_json::from_str(l).expect("parse chunk row"))
        .collect();
    let load_ms = t.elapsed().as_millis();
    println!("chunks: {}  ({load_ms} ms load)", chunks.len());

    // Stage 1: phrase extraction + df stats.
    let t = Instant::now();
    let mut per_chunk: Vec<HashMap<String, u32>> = Vec::with_capacity(chunks.len());
    let mut df: HashMap<String, u32> = HashMap::new();
    let mut tf: HashMap<String, u32> = HashMap::new();
    for c in &chunks {
        let phrases = extract_phrases(&c.content, &stop);
        for (p, n) in &phrases {
            *df.entry(p.clone()).or_insert(0) += 1;
            *tf.entry(p.clone()).or_insert(0) += n;
        }
        per_chunk.push(phrases);
    }
    let extract_ms = t.elapsed().as_millis();
    println!("candidate phrases: {}  ({extract_ms} ms extract)", df.len());

    // Stage 2: df-band prune + tf-idf top-V vocabulary.
    let t = Instant::now();
    let n_chunks = chunks.len() as f64;
    let mut scored: Vec<(String, f64)> = df
        .iter()
        .filter(|(_, &d)| d as usize >= MIN_DF && (d as f64) / n_chunks <= MAX_DF_FRAC)
        .map(|(p, &d)| {
            let t_f = tf[p] as f64;
            let idf = ((n_chunks + 1.0) / (d as f64 + 1.0)).ln();
            (p.clone(), t_f * idf)
        })
        .collect();
    scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    scored.truncate(TOP_V);
    let concept_id: HashMap<&str, u32> = scored
        .iter()
        .enumerate()
        .map(|(i, (p, _))| (p.as_str(), i as u32))
        .collect();
    let vocab_ms = t.elapsed().as_millis();
    println!("vocabulary: {} concepts  ({vocab_ms} ms)", concept_id.len());

    // Stage 3: chunk-window co-occurrence edges.
    let t = Instant::now();
    let mut edges: HashMap<(u32, u32), f64> = HashMap::new();
    let mut concept_chunks: HashMap<u32, Vec<u32>> = HashMap::new();
    for (ci, phrases) in per_chunk.iter().enumerate() {
        let mut present: Vec<(u32, f64)> = phrases
            .iter()
            .filter_map(|(p, &n)| concept_id.get(p.as_str()).map(|&id| (id, n as f64)))
            .collect();
        if present.len() > CHUNK_CONCEPT_CAP {
            present.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap());
            present.truncate(CHUNK_CONCEPT_CAP);
        }
        for &(id, _) in &present {
            concept_chunks.entry(id).or_default().push(ci as u32);
        }
        for i in 0..present.len() {
            for j in (i + 1)..present.len() {
                let (a, b) = (present[i].0.min(present[j].0), present[i].0.max(present[j].0));
                *edges.entry((a, b)).or_insert(0.0) += 1.0;
            }
        }
    }
    let pre_prune = edges.len();
    edges.retain(|_, w| *w >= MIN_COOC);
    // Normalize by concept df (cosine-style): raw counts let hub concepts
    // dominate modularity — run 1's largest community was a hub mush.
    let df_of: Vec<f64> = scored
        .iter()
        .map(|(p, _)| df[p.as_str()] as f64)
        .collect();
    for ((a, bn), w) in edges.iter_mut() {
        *w /= (df_of[*a as usize] * df_of[*bn as usize]).sqrt();
    }
    let edge_ms = t.elapsed().as_millis();
    println!("edges: {} (of {pre_prune} pre-prune)  ({edge_ms} ms)", edges.len());

    // Stage 4: Leiden.
    let t = Instant::now();
    let mut b = GraphDataBuilder::new(concept_id.len());
    for (&(a, bnode), &w) in &edges {
        b.add_edge(a as usize, bnode as usize, w)?;
    }
    let graph = b.build()?;
    let leiden = Leiden::new(LeidenConfig {
        resolution,
        seed: Some(7),
        ..Default::default()
    });
    let result = leiden.run(&graph).map_err(|e| format!("leiden: {e:?}"))?;
    let leiden_ms = t.elapsed().as_millis();
    let n_comm = result.partition.num_communities();
    println!("communities: {n_comm}  quality: {:.4}  ({leiden_ms} ms leiden)", result.quality);

    // Eyeball report: 20 largest communities, top member concepts + the
    // article titles their chunks concentrate in.
    let mut members: HashMap<usize, Vec<u32>> = HashMap::new();
    for (node, &comm) in result.partition.as_slice().iter().enumerate() {
        members.entry(comm).or_default().push(node as u32);
    }
    let mut by_size: Vec<(usize, Vec<u32>)> = members.into_iter().collect();
    by_size.sort_by_key(|(_, m)| std::cmp::Reverse(m.len()));

    let vocab: Vec<&str> = scored.iter().map(|(p, _)| p.as_str()).collect();
    let mut report = String::new();
    for (rank, (comm, mem)) in by_size.iter().take(20).enumerate() {
        // Concepts ranked by tf-idf order (vocab is already sorted).
        let mut ids = mem.clone();
        ids.sort();
        let top: Vec<&str> = ids.iter().take(10).map(|&i| vocab[i as usize]).collect();
        let mut title_hits: HashMap<&str, u32> = HashMap::new();
        for &cid in mem {
            if let Some(chs) = concept_chunks.get(&cid) {
                for &ci in chs {
                    *title_hits.entry(chunks[ci as usize].title.as_str()).or_insert(0) += 1;
                }
            }
        }
        let mut titles: Vec<(&str, u32)> = title_hits.into_iter().collect();
        titles.sort_by_key(|(_, n)| std::cmp::Reverse(*n));
        report.push_str(&format!(
            "#{:>2} community {} — {} concepts\n    concepts: {}\n    articles: {}\n",
            rank + 1,
            comm,
            mem.len(),
            top.join(" | "),
            titles
                .iter()
                .take(3)
                .map(|(t, n)| format!("{t} ({n})"))
                .collect::<Vec<_>>()
                .join(", "),
        ));
    }
    std::fs::create_dir_all(std::path::Path::new(&out_path).parent().unwrap())?;
    let mut f = std::fs::File::create(&out_path)?;
    writeln!(
        f,
        "SP5 concept-graph probe — {} chunks, {} concepts, {} edges, {} communities\n\
         timings ms: load={load_ms} extract={extract_ms} vocab={vocab_ms} edges={edge_ms} \
         leiden={leiden_ms} total={}\n",
        chunks.len(),
        concept_id.len(),
        edges.len(),
        n_comm,
        t_total.elapsed().as_millis(),
    )?;
    f.write_all(report.as_bytes())?;
    println!("\n{report}");
    println!(
        "TOTAL: {:.1}s (gate: < 300s)  report -> {out_path}",
        t_total.elapsed().as_secs_f64()
    );
    Ok(())
}
