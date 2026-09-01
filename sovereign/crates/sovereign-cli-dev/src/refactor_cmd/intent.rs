// SPDX-License-Identifier: AGPL-3.0-or-later
//! Duplicate INTENT — the reimplementation that neither a name census nor a
//! clone search can see.
//!
//! # The hole this fills
//!
//! Three detectors already look for duplication and each keys on something
//! different: [`DetectorId::Name`] on identity (the same type name in two
//! crates), [`DetectorId::Shape`] on structure (the renamed fork), and
//! [`DetectorId::Behaviour`] on the CODE (exact and near clones of the body).
//!
//! None of them can see eight implementations of cosine similarity written
//! four different ways. `Name` cannot — the names differ. `Shape` cannot —
//! these are functions, not type shapes. `Behaviour` cannot — the bodies are
//! genuinely different code, which is the whole point; a near-clone cutoff of
//! 0.95 over normalized text is looking for the copy-paste it did not come
//! from.
//!
//! What they share is the JOB, and since the code-intel pass every symbol
//! carries a plain-English statement of it: `summary` plus two `asks`. That is
//! the signal here.
//!
//! # The filter IS the detector, and that was measured, not reasoned
//!
//! Ranking symbols by intent similarity alone reproduces `Behaviour`'s output.
//! Measured 2026-08-31 over 23,886 summaries: of the top 200 intent-similar
//! pairs, **200 were the same name** — trait-impl boilerplate (`record`,
//! `neighbors`, `vocabulary`) that a clone search already reports. An
//! intent-ranked list is not a new instrument.
//!
//! Requiring a DIFFERENT name in a DIFFERENT crate is what makes it one. At
//! that cut the same corpus yields 92 candidate pairs over 94 symbols, and the
//! top of the list is `cosine_sim` / `cosine_similarity` / `cosine` /
//! `probe_cosine` across five crates, plus `indexes_dir` against
//! `sovereign_indexes` — two accessors for one path, which ARCH §10.6 forbids
//! by name.
//!
//! # Hubs, not pairs
//!
//! The same measurement said the output shape is wrong if it is pairwise: the
//! degree distribution is dominated by hubs, one concept implemented N times
//! (cosine appears in 10 of the top pairs). A worker wants "this job has eight
//! homes, pick an owner", not 28 rows saying the same thing. So members are
//! grouped by the discriminative terms they share.
//!
//! # Lexical, deliberately
//!
//! Measured 2026-08-31 on the same ground truth: IDF-weighted lexical overlap
//! scored 62.8/60.7 against embeddings' 60.5/62.5. The vector path buys
//! nothing here and would put a daemon round-trip inside a detector that has
//! to fit the close budget. Same scorer shape as
//! [`super::affinity`] — one decider (ARCH §10.6).

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::Path;

/// Frozen settings. Every one of these moves the number, so every one of them
/// is in the digest.
#[derive(Debug, Clone, Copy)]
pub struct IntentOptions {
    /// Cosine-on-IDF floor for a pair to be a candidate.
    pub min_score: f64,
    /// A pair must share at least this many discriminative terms. One shared
    /// rare word is a coincidence, not evidence.
    pub min_shared: usize,
    /// A term appearing in more than this many symbols is vocabulary, not a
    /// signal, and is dropped from the inverted index.
    pub rare_df: usize,
    /// Skip a posting list longer than this — it is a stop-word we failed to
    /// recognise, and it would dominate the O(postings²) pairing cost.
    pub max_postings: usize,
    /// Below this many usable terms a summary is too thin to compare.
    pub min_terms: usize,
}

impl Default for IntentOptions {
    fn default() -> Self {
        Self {
            min_score: 0.75,
            min_shared: 2,
            rare_df: 400,
            max_postings: 200,
            min_terms: 6,
        }
    }
}

impl IntentOptions {
    pub fn digest(&self) -> String {
        format!(
            "min_score={};min_shared={};rare_df={};max_postings={};min_terms={}",
            self.min_score, self.min_shared, self.rare_df, self.max_postings, self.min_terms
        )
    }
}

/// One symbol, reduced to what this detector compares.
#[derive(Debug, Clone)]
pub struct IntentSymbol {
    pub name: String,
    pub qualified_name: String,
    pub file: String,
    pub line: i32,
    pub krate: String,
    pub is_type: bool,
    terms: BTreeSet<String>,
}

/// One job with more than one home.
#[derive(Debug, Clone)]
pub struct IntentCluster {
    /// The discriminative terms the members share — this is the cluster's
    /// name, and it is what `token` is derived from.
    pub terms: Vec<String>,
    pub members: Vec<IntentSymbol>,
    /// Weakest pairwise score inside the cluster.
    pub min_score: f64,
}

const STOP: &[&str] = &[
    "the", "and", "for", "with", "into", "from", "this", "that", "are", "was", "any", "all",
    "not", "you", "your", "its", "which", "when", "what", "how", "does", "used", "use", "using",
    "returns", "return", "given", "based", "value", "values", "function", "method", "struct",
    "enum", "type", "types", "data", "code", "one", "two",
];

fn terms_of(text: &str) -> BTreeSet<String> {
    let stop: HashSet<&str> = STOP.iter().copied().collect();
    let mut out = BTreeSet::new();
    let mut cur = String::new();
    for ch in text.chars() {
        if ch.is_ascii_alphanumeric() || ch == '_' {
            cur.push(ch.to_ascii_lowercase());
        } else {
            if cur.len() >= 3 && cur.starts_with(|c: char| c.is_ascii_alphabetic()) {
                if !stop.contains(cur.as_str()) {
                    out.insert(std::mem::take(&mut cur));
                } else {
                    cur.clear();
                }
            } else {
                cur.clear();
            }
        }
    }
    if cur.len() >= 3 && cur.starts_with(|c: char| c.is_ascii_alphabetic()) && !stop.contains(cur.as_str())
    {
        out.insert(cur);
    }
    out
}

/// Which crate owns this path. `sovereign/crates/sovereign-core/src/x.rs` is
/// `sovereign-core`; `corpus-engine/src/x.rs` is `corpus-engine`. A path with
/// no `/src/` keeps its first component, which is right for `xtask` and
/// friends and never merges two real crates.
pub fn crate_of(file: &str) -> String {
    match file.split_once("/src/") {
        Some((head, _)) => head.rsplit('/').next().unwrap_or(head).to_string(),
        None => file.split('/').next().unwrap_or(file).to_string(),
    }
}

/// A path whose duplication is not ours to converge: vendored trees, research
/// scratch, and generated output. Same reasoning as `affinity::retain_tracked`
/// — prior art means "already exists HERE".
fn is_ours(file: &str) -> bool {
    !(file.starts_with("research/")
        || file.contains("/target/")
        || file.starts_with("target/")
        || file.contains("/vendor/"))
}

/// Read the code-intel summaries for a corpus.
///
/// `index_path` is the corpus index dir (`~/.svrnmesh/indexes/<id>`), the same
/// one [`dry_report`] is handed. Absent cache is `Ok(vec![])` and NOT an
/// error the caller should treat as "no duplication": the detector's control
/// site turns that into COULD-NOT-JUDGE, which is the honest verdict.
pub fn load_intent_corpus(index_path: &Path, opts: &IntentOptions) -> Result<Vec<IntentSymbol>, String> {
    let path = index_path.join("code_intel_cache.json");
    if !path.exists() {
        return Ok(Vec::new());
    }
    let raw = std::fs::read_to_string(&path).map_err(|e| format!("read {}: {e}", path.display()))?;
    let entries: Vec<serde_json::Value> =
        serde_json::from_str(&raw).map_err(|e| format!("parse {}: {e}", path.display()))?;

    let mut out = Vec::with_capacity(entries.len());
    for e in entries {
        let meta = &e["meta"];
        let file = meta["file_path"].as_str().unwrap_or_default().to_string();
        if file.is_empty() || !is_ours(&file) {
            continue;
        }
        let mut text = e["summary"].as_str().unwrap_or_default().to_string();
        if let Some(asks) = e["asks"].as_array() {
            for a in asks {
                text.push(' ');
                text.push_str(a.as_str().unwrap_or_default());
            }
        }
        let terms = terms_of(&text);
        if terms.len() < opts.min_terms {
            continue;
        }
        out.push(IntentSymbol {
            name: meta["name"].as_str().unwrap_or_default().to_string(),
            qualified_name: meta["qualified_name"].as_str().unwrap_or_default().to_string(),
            krate: crate_of(&file),
            file,
            line: meta["line_start"].as_i64().unwrap_or(0) as i32,
            is_type: e["cache_key"].as_str().unwrap_or_default().contains("/ty"),
            terms,
        });
    }
    Ok(out)
}

/// The WHOLE population, clustered. Never scoped — callers narrow the result
/// (`detector.rs` module doc: a cross-crate predicate handed one crate returns
/// zero for the wrong reason).
pub fn intent_census(symbols: &[IntentSymbol], opts: &IntentOptions) -> Vec<IntentCluster> {
    if symbols.len() < 2 {
        return Vec::new();
    }
    let n = symbols.len() as f64;
    let mut df: HashMap<&str, usize> = HashMap::new();
    for s in symbols {
        for t in &s.terms {
            *df.entry(t.as_str()).or_insert(0) += 1;
        }
    }
    let idf = |t: &str| -> f64 {
        let c = *df.get(t).unwrap_or(&0);
        (n / (1.0 + c as f64)).ln()
    };
    let norm: Vec<f64> = symbols
        .iter()
        .map(|s| s.terms.iter().map(|t| idf(t)).sum::<f64>().sqrt())
        .collect();

    // Block on discriminative terms so this is O(postings²) per rare term, not
    // O(symbols²). Behaviour's near tier is the O(n²) one and costs 156s;
    // this is what keeps Intent inside the close budget.
    let mut inv: HashMap<&str, Vec<usize>> = HashMap::new();
    for (i, s) in symbols.iter().enumerate() {
        for t in &s.terms {
            if *df.get(t.as_str()).unwrap_or(&0) <= opts.rare_df {
                inv.entry(t.as_str()).or_default().push(i);
            }
        }
    }

    let mut shared: HashMap<(usize, usize), usize> = HashMap::new();
    for (_, ids) in inv.iter() {
        if ids.len() > opts.max_postings {
            continue;
        }
        for a in 0..ids.len() {
            for b in (a + 1)..ids.len() {
                let (i, j) = (ids[a].min(ids[b]), ids[a].max(ids[b]));
                *shared.entry((i, j)).or_insert(0) += 1;
            }
        }
    }

    // Union-find over surviving pairs, so one job with eight homes is one
    // cluster rather than 28 rows.
    let mut parent: Vec<usize> = (0..symbols.len()).collect();
    fn find(parent: &mut Vec<usize>, x: usize) -> usize {
        let mut r = x;
        while parent[r] != r {
            r = parent[r];
        }
        let mut c = x;
        while parent[c] != c {
            let nx = parent[c];
            parent[c] = r;
            c = nx;
        }
        r
    }
    let mut pair_scores: Vec<((usize, usize), f64)> = Vec::new();
    for ((i, j), sh) in shared {
        if sh < opts.min_shared {
            continue;
        }
        let (a, b) = (&symbols[i], &symbols[j]);
        // The filter that makes this a distinct instrument, not a rerun of
        // Behaviour. Both halves are load-bearing; see the module doc.
        if a.name == b.name || a.krate == b.krate || a.is_type != b.is_type {
            continue;
        }
        if norm[i] == 0.0 || norm[j] == 0.0 {
            continue;
        }
        let inter: f64 = a.terms.intersection(&b.terms).map(|t| idf(t)).sum();
        let score = inter / (norm[i] * norm[j]);
        if score < opts.min_score {
            continue;
        }
        pair_scores.push(((i, j), score));
        let (ri, rj) = (find(&mut parent, i), find(&mut parent, j));
        if ri != rj {
            parent[ri] = rj;
        }
    }

    let mut groups: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
    let mut min_of: HashMap<usize, f64> = HashMap::new();
    for ((i, j), s) in &pair_scores {
        let root = find(&mut parent, *i);
        let e = min_of.entry(root).or_insert(f64::MAX);
        if *s < *e {
            *e = *s;
        }
        for k in [*i, *j] {
            let g = groups.entry(root).or_default();
            if !g.contains(&k) {
                g.push(k);
            }
        }
    }

    let mut out: Vec<IntentCluster> = groups
        .into_iter()
        .map(|(root, mut ids)| {
            ids.sort_by(|a, b| {
                symbols[*a]
                    .file
                    .cmp(&symbols[*b].file)
                    .then(symbols[*a].line.cmp(&symbols[*b].line))
            });
            // The cluster's name: terms carried by at least half the members,
            // ranked by IDF.
            //
            // NOT the intersection of all of them. `token()` is the control's
            // join key, and a strict intersection over a 22-member cluster is
            // emptied by ONE member whose summary words it differently — the
            // control would then go quiet because the vocabulary drifted, not
            // because the duplication was converged. That is precisely the
            // false COULD-NOT-JUDGE the control mechanism exists to avoid
            // (detector.rs, "The negative control").
            let mut freq: BTreeMap<&str, usize> = BTreeMap::new();
            for k in &ids {
                for t in &symbols[*k].terms {
                    *freq.entry(t.as_str()).or_insert(0) += 1;
                }
            }
            let quorum = ids.len().div_ceil(2);
            let mut terms: Vec<String> = freq
                .into_iter()
                .filter(|(_, c)| *c >= quorum)
                .map(|(t, _)| t.to_string())
                .collect();
            terms.sort_by(|a, b| {
                idf(b)
                    .partial_cmp(&idf(a))
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
            terms.truncate(3);
            terms.sort();
            IntentCluster {
                terms,
                members: ids.iter().map(|i| symbols[*i].clone()).collect(),
                min_score: *min_of.get(&root).unwrap_or(&0.0),
            }
        })
        .filter(|c| c.members.len() >= 2 && !c.terms.is_empty())
        .collect();
    out.sort_by(|a, b| {
        b.members
            .len()
            .cmp(&a.members.len())
            .then(b.min_score.partial_cmp(&a.min_score).unwrap_or(std::cmp::Ordering::Equal))
    });
    out
}

impl IntentCluster {
    /// The join key against the label store. Deterministic and greppable —
    /// `Site::key`'s contract is that a human can find this in a `.jsonl`.
    pub fn token(&self) -> String {
        format!("intent:{}", self.terms.join("+"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sym(name: &str, file: &str, text: &str) -> IntentSymbol {
        IntentSymbol {
            name: name.into(),
            qualified_name: format!("q/{name}"),
            krate: crate_of(file),
            file: file.into(),
            line: 1,
            is_type: false,
            terms: terms_of(text),
        }
    }

    fn opts() -> IntentOptions {
        IntentOptions {
            min_score: 0.5,
            min_shared: 2,
            rare_df: 400,
            max_postings: 200,
            min_terms: 3,
        }
    }

    /// The case the whole detector exists for: same job, different names,
    /// different crates, and code a clone search would never pair.
    #[test]
    fn a_job_implemented_twice_under_different_names_is_one_cluster() {
        let syms = vec![
            sym(
                "cosine_sim",
                "corpus-engine/src/a.rs",
                "Computes the cosine similarity between two embedding vectors.",
            ),
            sym(
                "probe_cosine",
                "sovereign/crates/sovereign-core/src/b.rs",
                "Computes the cosine similarity between two embedding vectors.",
            ),
        ];
        let c = intent_census(&syms, &opts());
        assert_eq!(c.len(), 1, "expected one cluster, got {c:?}");
        assert_eq!(c[0].members.len(), 2);
    }

    /// The filter's first half. Same name is `Behaviour`'s and `Name`'s
    /// territory; reporting it here would just duplicate another detector.
    #[test]
    fn the_same_name_in_two_crates_is_not_an_intent_finding() {
        let syms = vec![
            sym(
                "cosine_sim",
                "corpus-engine/src/a.rs",
                "Computes the cosine similarity between two embedding vectors.",
            ),
            sym(
                "cosine_sim",
                "sovereign/crates/sovereign-core/src/b.rs",
                "Computes the cosine similarity between two embedding vectors.",
            ),
        ];
        assert!(intent_census(&syms, &opts()).is_empty());
    }

    /// The filter's second half. Two helpers in one crate are that crate's
    /// business; this detector is about a concept escaping its home.
    #[test]
    fn two_names_inside_one_crate_are_not_an_intent_finding() {
        let syms = vec![
            sym(
                "cosine_sim",
                "corpus-engine/src/a.rs",
                "Computes the cosine similarity between two embedding vectors.",
            ),
            sym(
                "probe_cosine",
                "corpus-engine/src/b.rs",
                "Computes the cosine similarity between two embedding vectors.",
            ),
        ];
        assert!(intent_census(&syms, &opts()).is_empty());
    }

    /// Eight homes is ONE finding with eight members, not 28 pairs.
    #[test]
    fn one_job_with_many_homes_is_a_single_cluster() {
        let text = "Computes the cosine similarity between two embedding vectors.";
        let syms: Vec<_> = (0..5)
            .map(|i| {
                sym(
                    &format!("cos{i}"),
                    &format!("crate{i}/src/x.rs"),
                    text,
                )
            })
            .collect();
        let c = intent_census(&syms, &opts());
        assert_eq!(c.len(), 1, "one concept must be one cluster");
        assert_eq!(c[0].members.len(), 5);
    }

    /// One member wording it differently must not erase the cluster's name.
    /// The token is the control's join key, so an empty or shifting token is a
    /// control that goes quiet for the wrong reason.
    #[test]
    fn one_odd_member_does_not_empty_the_cluster_token() {
        let text = "Computes the cosine similarity between two embedding vectors.";
        let mut syms: Vec<_> = (0..4)
            .map(|i| sym(&format!("cos{i}"), &format!("crate{i}/src/x.rs"), text))
            .collect();
        // Shares enough to join, words the rest of it completely differently.
        syms.push(sym(
            "odd_one",
            "crateX/src/x.rs",
            "Cosine similarity over vectors, expressed with wholly different prose.",
        ));
        let c = intent_census(&syms, &opts());
        assert_eq!(c.len(), 1, "expected one cluster, got {c:?}");
        assert!(
            !c[0].terms.is_empty(),
            "a quorum token must survive one differently-worded member"
        );
        assert!(c[0].token().len() > "intent:".len());
    }

    #[test]
    fn crate_of_reads_both_workspace_layouts() {
        assert_eq!(crate_of("corpus-engine/src/facts_check.rs"), "corpus-engine");
        assert_eq!(
            crate_of("sovereign/crates/sovereign-core/src/memory.rs"),
            "sovereign-core"
        );
    }

    /// Vendored and research trees are not ours to converge.
    #[test]
    fn vendored_and_research_paths_are_excluded() {
        assert!(!is_ours("research/verifier-v0/data/llama.cpp/x.py"));
        assert!(!is_ours("corpus-engine/target/debug/build/x.rs"));
        assert!(is_ours("corpus-engine/src/lib.rs"));
    }
}

/// Render for `svrn code converge verb`.
///
/// Deliberately shaped like a `converge` dossier and not like a diff: the
/// worker's question is "which of these homes should own this job", so the
/// crates are the headline and the bodies are not shown at all.
pub fn render_intent(clusters: &[IntentCluster], limit: usize) -> String {
    let mut out = String::new();
    let shown = if limit == 0 {
        clusters.len()
    } else {
        limit.min(clusters.len())
    };
    let homes: usize = clusters.iter().map(|c| c.members.len()).sum();
    out.push_str(&format!(
        "# converge verb — duplicated INTENT\n\n\
         **{}** job(s) with more than one home, **{}** implementations total.\n\n\
         Same job, different name, different crate. A name census cannot see \
         these (the names differ) and a clone search cannot (the code does). \
         Advisory: the ranker is lexical over behaviour descriptions, so this \
         is a shortlist for adjudication, never a verdict.\n\n",
        clusters.len(),
        homes
    ));
    if clusters.is_empty() {
        out.push_str("Nothing above the score floor.\n");
        return out;
    }
    for c in clusters.iter().take(shown) {
        let crates: BTreeSet<&str> = c.members.iter().map(|m| m.krate.as_str()).collect();
        out.push_str(&format!(
            "- **{} homes across {} crates** · `{}` · min {:.3}\n",
            c.members.len(),
            crates.len(),
            c.token(),
            c.min_score
        ));
        for m in &c.members {
            out.push_str(&format!(
                "  - `{}` — {}:{}  [{}]\n",
                m.name, m.file, m.line, m.krate
            ));
        }
    }
    if shown < clusters.len() {
        out.push_str(&format!(
            "\n… and {} more (--limit 0 for all)\n",
            clusters.len() - shown
        ));
    }
    out
}
