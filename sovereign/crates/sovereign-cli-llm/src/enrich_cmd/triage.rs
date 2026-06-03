//! `sovereign enrich triage-candidates <atlas-corpus>` — placeholder +
//! in-corpus degree distribution from a structure_first atlas.
//!
//! Emits two histograms + two top-K tables so an operator can decide:
//!   - Which **placeholder entities** are worth a Tier-1.5
//!     classification pass (high inbound degree = many in-corpus
//!     articles point at this off-corpus title; classifying it
//!     surfaces a richer brief without re-ingesting the article).
//!   - Which **in-corpus articles** are worth a Tier-2 full
//!     extraction pass (high inbound + outbound degree = central in
//!     the link graph; the deeper atom set lifts retrieval most for
//!     these).
//!
//! No LLM, no daemon — pure read of `atlas/atoms.json` + `atlas/edges.json`.

use std::collections::HashMap;

use corpus_engine::enrichment::atlas::{
    read_atlas_atoms, read_atlas_edges, AtomEnvelope, ATLAS_DIRNAME,
};

use super::paths;
use crate::util::help::{self, Help, HelpSection};

const HELP: Help = Help {
    command: "sovereign enrich triage-candidates",
    summary: "Rank atlas entities by inbound link degree to pick Tier-1.5 / Tier-2 enrichment candidates.",
    sections: &[
        HelpSection::Usage(
            "sovereign enrich triage-candidates <atlas-corpus> [--top-k N] [--json]",
        ),
        HelpSection::Flags(&[
            (
                "--top-k <N>",
                "How many entries to print per top-K table (default 25).",
            ),
            (
                "--json",
                "Emit the full distribution as JSON on stdout instead of the human-readable tables.",
            ),
        ]),
        HelpSection::Examples(&[
            (
                "sovereign enrich triage-candidates wiki-l5-struct --top-k 50",
                "Top-50 placeholders + top-50 in-corpus by centrality.",
            ),
        ]),
        HelpSection::Notes(
            "Reads atlas/atoms.json + atlas/edges.json under ~/.sovereign/indexes/<atlas-corpus>/. \
             Placeholder = entity with empty description (off-corpus wikilink target). \
             A placeholder's high inbound degree means many in-corpus articles reference it — \
             prime candidate for Tier-1.5 classification (entity_type + 1-line description \
             from title alone, ~1-3s LLM call each).",
        ),
    ],
};

pub async fn cmd_triage(args: &[String]) -> i32 {
    if help::wants_help(args) {
        help::print(&HELP);
        return 0;
    }
    let parsed = match parse_args(args) {
        Ok(p) => p,
        Err(msg) => {
            eprintln!("error: {msg}");
            eprintln!();
            help::print(&HELP);
            return 2;
        }
    };
    let Some(corpus_id) = parsed.corpus_id.as_deref() else {
        eprintln!("error: missing <atlas-corpus> id");
        return 2;
    };

    let atlas_dir = paths::index_root(corpus_id).join(ATLAS_DIRNAME);
    if !atlas_dir.exists() {
        eprintln!(
            "error: no atlas at {} — run `sovereign enrich ingest {corpus_id} --strategy structure_first --source-corpus <id>` first",
            atlas_dir.display()
        );
        return 1;
    }

    let atoms_file = match read_atlas_atoms(&atlas_dir) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("error: reading atoms.json: {e}");
            return 1;
        }
    };
    let edges_file = match read_atlas_edges(&atlas_dir) {
        Ok(e) => e,
        Err(e) => {
            eprintln!("error: reading edges.json: {e}");
            return 1;
        }
    };

    // Build entity index. Track which are placeholders (empty
    // description, salience 0.0 — the structure_first hallmark for
    // off-corpus wikilink targets).
    #[derive(Debug, Clone)]
    struct EntityRef {
        canonical_name: String,
        is_placeholder: bool,
    }
    let mut entities: HashMap<String, EntityRef> = HashMap::new();
    for atom in &atoms_file.atoms {
        if let AtomEnvelope::Entity(e) = atom {
            let is_placeholder = e.description.is_empty() && e.salience == 0.0;
            entities.insert(
                e.id.as_str().to_string(),
                EntityRef {
                    canonical_name: e.canonical_name.clone(),
                    is_placeholder,
                },
            );
        }
    }
    let total_entities = entities.len();
    let placeholder_count = entities.values().filter(|e| e.is_placeholder).count();
    let in_corpus_count = total_entities - placeholder_count;

    // Inbound + outbound degree. Use BTreeMap for stable output.
    let mut inbound: HashMap<String, u32> = HashMap::with_capacity(entities.len());
    let mut outbound: HashMap<String, u32> = HashMap::with_capacity(entities.len());
    for edge in &edges_file.edges {
        *outbound
            .entry(edge.source.as_str().to_string())
            .or_insert(0) += 1;
        *inbound.entry(edge.target.as_str().to_string()).or_insert(0) += 1;
    }

    // Per-bucket histograms.
    let bucket_for = |d: u32| match d {
        0 => "0",
        1 => "1",
        2..=5 => "2-5",
        6..=10 => "6-10",
        11..=50 => "11-50",
        51..=100 => "51-100",
        101..=500 => "101-500",
        _ => "501+",
    };
    let mut placeholder_inbound_hist: HashMap<&'static str, u64> = HashMap::new();
    let mut in_corpus_inbound_hist: HashMap<&'static str, u64> = HashMap::new();
    let mut in_corpus_outbound_hist: HashMap<&'static str, u64> = HashMap::new();

    for (id, e) in &entities {
        let inb = *inbound.get(id).unwrap_or(&0);
        let outb = *outbound.get(id).unwrap_or(&0);
        if e.is_placeholder {
            *placeholder_inbound_hist.entry(bucket_for(inb)).or_insert(0) += 1;
        } else {
            *in_corpus_inbound_hist.entry(bucket_for(inb)).or_insert(0) += 1;
            *in_corpus_outbound_hist.entry(bucket_for(outb)).or_insert(0) += 1;
        }
    }

    // Top-K rankings.
    let mut placeholder_ranked: Vec<(String, u32)> = entities
        .iter()
        .filter(|(_, e)| e.is_placeholder)
        .map(|(id, e)| (e.canonical_name.clone(), *inbound.get(id).unwrap_or(&0)))
        .collect();
    placeholder_ranked.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));

    let mut in_corpus_ranked: Vec<(String, u32, u32)> = entities
        .iter()
        .filter(|(_, e)| !e.is_placeholder)
        .map(|(id, e)| {
            (
                e.canonical_name.clone(),
                *inbound.get(id).unwrap_or(&0),
                *outbound.get(id).unwrap_or(&0),
            )
        })
        .collect();
    // Centrality score = inbound + outbound (treat both directions equally
    // for triage; high either way is informative).
    in_corpus_ranked.sort_by(|a, b| {
        let sa = a.1 + a.2;
        let sb = b.1 + b.2;
        sb.cmp(&sa).then_with(|| a.0.cmp(&b.0))
    });

    if parsed.json {
        let payload = serde_json::json!({
            "atlas_corpus": corpus_id,
            "totals": {
                "entities_total": total_entities,
                "in_corpus": in_corpus_count,
                "placeholders": placeholder_count,
                "edges": edges_file.edges.len(),
            },
            "histograms": {
                "placeholder_inbound": bucket_sorted(&placeholder_inbound_hist),
                "in_corpus_inbound": bucket_sorted(&in_corpus_inbound_hist),
                "in_corpus_outbound": bucket_sorted(&in_corpus_outbound_hist),
            },
            "top_placeholders_by_inbound": placeholder_ranked
                .iter()
                .take(parsed.top_k)
                .map(|(n, i)| serde_json::json!({"name": n, "inbound": i}))
                .collect::<Vec<_>>(),
            "top_in_corpus_by_centrality": in_corpus_ranked
                .iter()
                .take(parsed.top_k)
                .map(|(n, i, o)| serde_json::json!({"name": n, "inbound": i, "outbound": o, "centrality": i + o}))
                .collect::<Vec<_>>(),
        });
        println!("{}", serde_json::to_string_pretty(&payload).unwrap());
        return 0;
    }

    println!("Atlas: {corpus_id}");
    println!("  entities_total : {total_entities}");
    println!("  in_corpus      : {in_corpus_count}");
    println!("  placeholders   : {placeholder_count}");
    println!("  edges          : {}", edges_file.edges.len());
    println!();
    println!("Placeholder inbound-degree histogram (Tier-1.5 candidates):");
    print_hist(&placeholder_inbound_hist, placeholder_count as u64);
    println!();
    println!("In-corpus inbound-degree histogram (incoming wikilinks from kept articles):");
    print_hist(&in_corpus_inbound_hist, in_corpus_count as u64);
    println!();
    println!("In-corpus outbound-degree histogram:");
    print_hist(&in_corpus_outbound_hist, in_corpus_count as u64);
    println!();
    println!(
        "Top-{} placeholders by inbound (Tier-1.5 budget guide):",
        parsed.top_k
    );
    for (i, (name, inb)) in placeholder_ranked.iter().take(parsed.top_k).enumerate() {
        println!("  {:>3}. {:>5} ← {}", i + 1, inb, name);
    }
    println!();
    println!(
        "Top-{} in-corpus by centrality (Tier-2 budget guide):",
        parsed.top_k
    );
    for (i, (name, inb, outb)) in in_corpus_ranked.iter().take(parsed.top_k).enumerate() {
        println!(
            "  {:>3}. centrality={:>5} (in={:>4}, out={:>4})  {}",
            i + 1,
            inb + outb,
            inb,
            outb,
            name
        );
    }

    0
}

fn print_hist(hist: &HashMap<&'static str, u64>, total: u64) {
    let order = [
        "0", "1", "2-5", "6-10", "11-50", "51-100", "101-500", "501+",
    ];
    let max = hist.values().copied().max().unwrap_or(0).max(1);
    for bucket in order {
        let count = hist.get(bucket).copied().unwrap_or(0);
        let pct = if total > 0 {
            (count as f64) * 100.0 / (total as f64)
        } else {
            0.0
        };
        let bar_len = ((count as f64) / (max as f64) * 40.0).round() as usize;
        let bar = "█".repeat(bar_len);
        println!("  {:>8}  {:>9}  {:>5.1}%  {}", bucket, count, pct, bar);
    }
}

fn bucket_sorted(hist: &HashMap<&'static str, u64>) -> Vec<serde_json::Value> {
    let order = [
        "0", "1", "2-5", "6-10", "11-50", "51-100", "101-500", "501+",
    ];
    order
        .iter()
        .map(|b| serde_json::json!({"bucket": *b, "count": hist.get(*b).copied().unwrap_or(0)}))
        .collect()
}

#[derive(Debug)]
struct ParsedTriage {
    corpus_id: Option<String>,
    top_k: usize,
    json: bool,
}

fn parse_args(args: &[String]) -> Result<ParsedTriage, String> {
    let mut out = ParsedTriage {
        corpus_id: None,
        top_k: 25,
        json: false,
    };
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--top-k" => {
                let v = args
                    .get(i + 1)
                    .ok_or("--top-k requires a value".to_string())?;
                let n: usize = v
                    .parse()
                    .map_err(|e| format!("--top-k must be a positive integer: {e}"))?;
                if n == 0 {
                    return Err("--top-k must be > 0".into());
                }
                out.top_k = n;
                i += 2;
            }
            "--json" => {
                out.json = true;
                i += 1;
            }
            other if other.starts_with("--") => {
                return Err(format!("unknown flag: {other}"));
            }
            other => {
                if out.corpus_id.is_none() {
                    out.corpus_id = Some(other.to_string());
                    i += 1;
                } else {
                    return Err(format!("unexpected positional argument: {other}"));
                }
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_minimal_invocation() {
        let p = parse_args(&["wiki-l5-struct".into()]).unwrap();
        assert_eq!(p.corpus_id.as_deref(), Some("wiki-l5-struct"));
        assert_eq!(p.top_k, 25);
        assert!(!p.json);
    }

    #[test]
    fn parse_top_k_and_json() {
        let p = parse_args(&[
            "wiki-l5-struct".into(),
            "--top-k".into(),
            "100".into(),
            "--json".into(),
        ])
        .unwrap();
        assert_eq!(p.top_k, 100);
        assert!(p.json);
    }

    #[test]
    fn parse_rejects_zero_top_k() {
        let err = parse_args(&["c".into(), "--top-k".into(), "0".into()]).unwrap_err();
        assert!(err.contains("> 0"));
    }
}
