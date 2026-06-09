// SPDX-License-Identifier: AGPL-3.0-or-later
//! Filesystem discovery for `sovereign bench all`.
//!
//! Walks `sovereign/bench/<group>/` looking for two surface shapes:
//!
//! - **Enrichment golden** — TOML with `[meta]` block + at least one
//!   populated `expected_*_atoms` array. Scored via
//!   `enrich_cmd::eval::score_corpus`.
//! - **Retrieval question bank** — TOML with `[bank]` table +
//!   `[[questions]]` array. Scored via `eval_cmd::runner::run_bank`.
//!
//! Anything that doesn't match either shape is silently skipped
//! (voice harness fragments, retired schemas, README markdown, …).
//!
//! The walk is content-sniffed via `toml::Value`, not via the heavier
//! `GoldenSet` / `EvalBank` deserialisers — `bench all` should report
//! "I found 7 enrichment goldens and 3 retrieval banks" even when one
//! of them won't parse cleanly under strict validation.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use corpus_engine::enrichment::atlas::axis_catalog::{all_axes, TypedAxis};

use crate::enrich_cmd::paths::index_root;

/// Which scoring surface a discovered bench belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BenchSurface {
    /// Enrichment-eval: atoms.json scored against a `GoldenSet` TOML.
    Enrichment,
    /// Retrieval + LLM-judge Q/A: live retrieval scored against a
    /// `[bank]` + `[[questions]]` TOML.
    RetrievalJudge,
}

impl BenchSurface {
    pub fn label(self) -> &'static str {
        match self {
            BenchSurface::Enrichment => "enrichment",
            BenchSurface::RetrievalJudge => "retrieval",
        }
    }
}

/// Atlas / index state for a discovered bench's corpus. Drives the
/// report's stale-vs-ready grading.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CorpusState {
    /// Atlas dir present with at least atoms.json. Enrichment lane
    /// can score; retrieval lane will score against the live daemon.
    Ready,
    /// Index dir exists but atlas is missing. Retrieval lane can
    /// still attempt to score (bm25 / vector against the chunks).
    /// Enrichment lane will mark this stale.
    IndexedNoAtlas,
    /// Corpus dir doesn't exist locally. Both surfaces mark stale.
    Unindexed,
}

impl CorpusState {
    pub fn is_ready_for(self, surface: BenchSurface) -> bool {
        matches!(
            (self, surface),
            (CorpusState::Ready, _) | (CorpusState::IndexedNoAtlas, BenchSurface::RetrievalJudge)
        )
    }
}

/// One bench the runner will attempt to score.
#[derive(Debug, Clone)]
pub struct DiscoveredBench {
    /// Stable id — filename stem. Used in the scoreboard, baseline
    /// path, `--filter` matching.
    pub id: String,
    /// Group dir name (`obsidian`, `literary`, `philosophy`,
    /// `sep`, `wikipedia`, …).
    pub group: String,
    /// Which scoring surface this bench belongs to.
    pub surface: BenchSurface,
    /// Absolute path to the TOML.
    pub bench_path: PathBuf,
    /// Corpus this bench scores against (atom store + index lookup).
    pub corpus_id: String,
    /// Whether `corpus_id` came from the TOML (explicit) or was
    /// inferred from the filename stem (implicit). Surfaced in the
    /// report as a warn so the author can add `[meta] corpus_id` to
    /// silence the inference.
    pub corpus_id_source: CorpusIdSource,
    /// Atlas / index state for `corpus_id`.
    pub corpus_state: CorpusState,
    /// Levers this bench covers. For Enrichment, the
    /// `expected_*_atoms` field names mapped to axis keys (catalog
    /// keys when known, raw kind name otherwise). For
    /// RetrievalJudge, the distinct `category` strings across
    /// `[[questions]]`.
    pub levers: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CorpusIdSource {
    /// Read from `[meta] corpus_id` (enrichment) or `[bank] corpus`
    /// (retrieval).
    Explicit,
    /// Inferred from the filename stem because the TOML didn't carry
    /// the field.
    InferredFromFilename,
}

/// Walk `bench_root` recursively (max-depth 3) and return every
/// `.toml` that classifies as one of the two surfaces. Order is
/// stable: alphabetical by `(group, id)`.
pub fn discover_benches(bench_root: &Path) -> Vec<DiscoveredBench> {
    let mut out = Vec::new();
    let Ok(groups) = fs::read_dir(bench_root) else {
        return out;
    };
    for group_entry in groups.flatten() {
        let group_path = group_entry.path();
        if !group_path.is_dir() {
            continue;
        }
        let group_name = group_path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_string();
        // Walk one level into each group dir. Bench files live at
        // `<group>/<bench>.toml`; deeper TOMLs (baselines/*.json,
        // for instance) are not bench definitions.
        let Ok(entries) = fs::read_dir(&group_path) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            // Follow the questions.toml symlinks in sep/ and
            // wikipedia/ — they ARE the bench definition.
            let is_toml = path
                .extension()
                .and_then(|e| e.to_str())
                .map(|e| e.eq_ignore_ascii_case("toml"))
                .unwrap_or(false);
            if !is_toml {
                continue;
            }
            if let Some(b) = classify(&path, &group_name) {
                out.push(b);
            }
        }
    }
    out.sort_by(|a, b| a.group.cmp(&b.group).then_with(|| a.id.cmp(&b.id)));
    out
}

fn classify(path: &Path, group: &str) -> Option<DiscoveredBench> {
    let bytes = fs::read_to_string(path).ok()?;
    let val: toml::Value = toml::from_str(&bytes).ok()?;

    let id = path.file_stem()?.to_str()?.to_string();
    let bench_path = path.to_path_buf();

    // ── Retrieval question bank ─────────────────────────────────
    //
    // Distinguishing shape: `[bank]` table + `[[questions]]` array.
    if let Some(bank) = val.get("bank").and_then(|v| v.as_table()) {
        let corpus = bank.get("corpus").and_then(|v| v.as_str())?;
        let (corpus_id, corpus_id_source) = if corpus.trim().is_empty() {
            (id.clone(), CorpusIdSource::InferredFromFilename)
        } else {
            (corpus.to_string(), CorpusIdSource::Explicit)
        };

        let categories: BTreeSet<String> = val
            .get("questions")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|q| q.as_table()?.get("category")?.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();

        return Some(DiscoveredBench {
            id,
            group: group.to_string(),
            surface: BenchSurface::RetrievalJudge,
            bench_path,
            corpus_id: corpus_id.clone(),
            corpus_id_source,
            corpus_state: inspect_corpus_state(&corpus_id),
            levers: categories.into_iter().collect(),
        });
    }

    // ── Enrichment golden ────────────────────────────────────────
    //
    // Distinguishing shape: top-level `expected_*_atoms` arrays.
    // `[meta] corpus_id` is preferred; filename stem is fallback.
    let levers = enrichment_levers(&val);
    if levers.is_empty() {
        return None;
    }
    let meta = val.get("meta").and_then(|v| v.as_table());
    let (corpus_id, corpus_id_source) = meta
        .and_then(|m| m.get("corpus_id").and_then(|v| v.as_str()))
        .filter(|s| !s.trim().is_empty())
        .map(|s| (s.to_string(), CorpusIdSource::Explicit))
        .unwrap_or_else(|| (id.clone(), CorpusIdSource::InferredFromFilename));

    Some(DiscoveredBench {
        id,
        group: group.to_string(),
        surface: BenchSurface::Enrichment,
        bench_path,
        corpus_id: corpus_id.clone(),
        corpus_id_source,
        corpus_state: inspect_corpus_state(&corpus_id),
        levers,
    })
}

/// Enrichment-lane lever extraction. Returns axis keys from the
/// catalog when the golden's field name matches a typed axis;
/// otherwise returns the base atom kind (person, concept, event,
/// etc.). Order matches catalog order for typed axes, then base
/// kinds alphabetically.
fn enrichment_levers(val: &toml::Value) -> Vec<String> {
    let table = match val.as_table() {
        Some(t) => t,
        None => return Vec::new(),
    };
    let mut out = Vec::new();

    // Catalog-driven typed axes come first.
    for axis in all_axes() {
        let field = field_name_for_axis(axis);
        if field_is_populated(table, &field) {
            out.push(axis.key.to_string());
        }
    }

    // Base atom-kind fields (not in the catalog but still scoring
    // signals). Order: alphabetical, deduped against catalog keys.
    let base_kinds = [
        "person",
        "concept",
        "work",
        "event",
        "state",
        "relation",
        "question",
        "claim",
        "fault_line",
        "open_question",
        "configuration",
        "position",
    ];
    for kind in base_kinds {
        let field = format!("expected_{kind}_atoms");
        if field_is_populated(table, &field) && !out.iter().any(|k| k == kind) {
            out.push(kind.to_string());
        }
    }

    out
}

fn field_name_for_axis(axis: &TypedAxis) -> String {
    // Catalog keys are snake_case identifiers matching the
    // golden's TOML field names by convention.
    format!("expected_{}_atoms", axis.key)
}

fn field_is_populated(table: &toml::map::Map<String, toml::Value>, field: &str) -> bool {
    table
        .get(field)
        .and_then(|v| v.as_array())
        .map(|a| !a.is_empty())
        .unwrap_or(false)
}

/// Resolve a corpus_id to its atlas / index state on disk.
pub fn inspect_corpus_state(corpus_id: &str) -> CorpusState {
    let idx = index_root(corpus_id);
    if !idx.exists() {
        return CorpusState::Unindexed;
    }
    let atoms = idx.join("atlas").join("atoms.json");
    if atoms.exists() {
        CorpusState::Ready
    } else {
        CorpusState::IndexedNoAtlas
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::TempDir;

    fn write_toml(dir: &Path, name: &str, body: &str) -> PathBuf {
        let path = dir.join(name);
        let mut f = fs::File::create(&path).unwrap();
        f.write_all(body.as_bytes()).unwrap();
        path
    }

    #[test]
    fn discover_retrieval_bank() {
        let tmp = TempDir::new().unwrap();
        let grp = tmp.path().join("sep");
        fs::create_dir_all(&grp).unwrap();
        write_toml(
            &grp,
            "questions.toml",
            r#"
[bank]
name = "sep-core-v1"
corpus = "sep"

[[questions]]
id = "q1"
category = "argument_reconstruction"
question = "Reconstruct van Inwagen's argument."

[[questions]]
id = "q2"
category = "concept_distinction"
question = "What distinguishes hard incompatibilism from libertarianism?"
"#,
        );
        let benches = discover_benches(tmp.path());
        assert_eq!(benches.len(), 1);
        assert_eq!(benches[0].surface, BenchSurface::RetrievalJudge);
        assert_eq!(benches[0].corpus_id, "sep");
        assert_eq!(benches[0].corpus_id_source, CorpusIdSource::Explicit);
        // Categories alphabetised by BTreeSet.
        assert_eq!(
            benches[0].levers,
            vec!["argument_reconstruction", "concept_distinction"]
        );
    }

    #[test]
    fn discover_enrichment_golden_with_explicit_corpus_id() {
        let tmp = TempDir::new().unwrap();
        let grp = tmp.path().join("obsidian");
        fs::create_dir_all(&grp).unwrap();
        write_toml(
            &grp,
            "golden.toml",
            r#"
[meta]
template = "obs-test"
corpus_id = "obsidian-vault"

[[expected_person_atoms]]
canonical_name_contains_any = ["Jacobs"]

[[expected_mechanism_atoms]]
name_contains_any = ["spread pricing"]
"#,
        );
        let benches = discover_benches(tmp.path());
        assert_eq!(benches.len(), 1);
        assert_eq!(benches[0].surface, BenchSurface::Enrichment);
        assert_eq!(benches[0].corpus_id, "obsidian-vault");
        assert_eq!(benches[0].corpus_id_source, CorpusIdSource::Explicit);
        // catalog axis (mechanism) listed before base kind (person)
        assert_eq!(benches[0].levers, vec!["mechanism", "person"]);
    }

    #[test]
    fn discover_falls_back_to_filename_stem() {
        let tmp = TempDir::new().unwrap();
        let grp = tmp.path().join("literary");
        fs::create_dir_all(&grp).unwrap();
        write_toml(
            &grp,
            "bk-book-1.toml",
            r#"
[meta]
template = "bk-book-1"

[[expected_person_atoms]]
canonical_name_contains_any = ["Karamazov"]
"#,
        );
        let benches = discover_benches(tmp.path());
        assert_eq!(benches.len(), 1);
        assert_eq!(benches[0].corpus_id, "bk-book-1");
        assert_eq!(
            benches[0].corpus_id_source,
            CorpusIdSource::InferredFromFilename
        );
    }

    #[test]
    fn discover_skips_non_bench_toml() {
        let tmp = TempDir::new().unwrap();
        let grp = tmp.path().join("voice");
        fs::create_dir_all(&grp).unwrap();
        // Voice harness shape — has neither [bank] nor expected_*_atoms.
        write_toml(
            &grp,
            "01-thing.toml",
            r#"
[scenario]
id = "01-thing"
contract = "relational"

[[turns]]
role = "user"
text = "hello"
"#,
        );
        // README-shaped TOML (not parseable as either surface).
        write_toml(&grp, "trash.toml", "this is = not [valid toml }");

        let benches = discover_benches(tmp.path());
        assert!(benches.is_empty());
    }

    #[test]
    fn discover_sorts_stable() {
        let tmp = TempDir::new().unwrap();
        for (group, name) in &[
            ("literary", "dubliners-3"),
            ("literary", "bk-book-1"),
            ("philosophy", "stoicism-mini"),
            ("philosophy", "free-will-debate"),
        ] {
            let grp = tmp.path().join(group);
            fs::create_dir_all(&grp).unwrap();
            write_toml(
                &grp,
                &format!("{name}.toml"),
                &format!(
                    r#"
[meta]
template = "{name}"
corpus_id = "{name}"

[[expected_person_atoms]]
canonical_name_contains_any = ["x"]
"#
                ),
            );
        }
        let benches = discover_benches(tmp.path());
        let ids: Vec<_> = benches
            .iter()
            .map(|b| (b.group.as_str(), b.id.as_str()))
            .collect();
        assert_eq!(
            ids,
            vec![
                ("literary", "bk-book-1"),
                ("literary", "dubliners-3"),
                ("philosophy", "free-will-debate"),
                ("philosophy", "stoicism-mini"),
            ]
        );
    }
}
