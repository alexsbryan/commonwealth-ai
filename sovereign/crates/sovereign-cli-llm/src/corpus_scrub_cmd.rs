//! `sovereign corpus scrub` — surface classified entities from a
//! corpus's atlas + apply an EntityMap to bench TOMLs.
//!
//! Two modes, selected by flags:
//!
//! 1. **Candidates extraction (default).** Reads the target corpus's
//!    `atoms.json` (produced by the atlas enrichment pipeline) and
//!    emits Entity atoms whose `entity_type` is person- or org-
//!    shaped, ranked by salience. No regex, no NER heuristics — the
//!    classification has already been done by the Phase 1 + Phase 3
//!    pipeline that runs at ingest time. Pre-condition: enrichment
//!    must be enabled on the corpus recipe so atoms.json exists.
//!
//! 2. **Apply mode (`--apply-to <bank.toml>`).** Loads an existing
//!    `EntityMap` JSON file (`--map <path>`) and rewrites every
//!    string in the bench bank TOML through
//!    `corpus_engine::pii::scrub_pii`. Writes in place after taking
//!    a `.bak` of the original.
//!
//! Why not regex over raw chunk text: that approach (which this
//! command originally shipped with) reinvents NER badly — a
//! capitalised-n-gram extractor with a stop-list either over-
//! filters and misses real entities, or under-filters and buries
//! signal in sentence-starter noise. The atlas pipeline does it
//! correctly (LLM classification + canonical-name resolution +
//! salience) and writes the result to atoms.json. Consume that.

use std::path::{Path, PathBuf};

use corpus_engine::pii::{scrub_pii, EntityMap};
use serde::{Deserialize, Serialize};

use crate::util::help::{self, Help, HelpSection};

const HELP: Help = Help {
    command: "sovereign corpus scrub",
    summary: "Surface classified entities from a corpus's atlas + apply EntityMap to bench TOMLs.",
    sections: &[
        HelpSection::Usage(
            "sovereign corpus scrub <corpus_id> [--out <path>] [--min-salience <f>]\n\
             sovereign corpus scrub --apply-to <bank.toml> --map <map.json>",
        ),
        HelpSection::Flags(&[
            (
                "<corpus_id>",
                "Corpus to read atoms.json from. Resolved as ~/.sovereign/indexes/<id>/atlas/atoms.json.",
            ),
            (
                "--out <path>",
                "Where to write the candidates JSON. Default: ~/.sovereign/conversations/entity-candidates.json",
            ),
            (
                "--min-salience <f>",
                "Drop entities below this salience score (0.0-1.0). Default 0.0 (keep all).",
            ),
            (
                "--include-concepts",
                "Also surface entity_type=concept atoms. Default: persons + orgs only.",
            ),
            (
                "--apply-to <bank.toml>",
                "Apply mode: rewrite bench bank through scrub_pii using --map.",
            ),
            (
                "--map <path>",
                "EntityMap JSON to load (apply mode only). Default: ~/.sovereign/conversations/entity-map.json",
            ),
        ]),
        HelpSection::Notes(
            "Candidates mode requires enrichment to have run on the corpus — atoms.json must \
             exist. If it doesn't, enable `[enrichment] enabled = true` on the recipe, re-ingest, \
             and re-run scrub. Output ranks by salience and dedupes on canonical_name.\n\n\
             The corpus itself is never modified; this command only produces derived artifacts.",
        ),
    ],
};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EntityCandidate {
    pub surface: String,
    pub kind_hint: String,
    pub salience: f32,
    pub description: String,
}

#[derive(Debug, Deserialize)]
struct AtomsFile {
    #[serde(default)]
    atoms: Vec<RawAtom>,
}

#[derive(Debug, Deserialize)]
struct RawAtom {
    #[serde(default)]
    atom_type: String,
    #[serde(default)]
    data: serde_json::Value,
}

pub async fn run_scrub(args: &[String]) -> i32 {
    if args.iter().any(|a| matches!(a.as_str(), "--help" | "-h")) {
        help::print(&HELP);
        return 0;
    }

    let mut corpus_id: Option<String> = None;
    let mut out: Option<PathBuf> = None;
    let mut min_salience: f32 = 0.0;
    let mut include_concepts = false;
    let mut apply_to: Option<PathBuf> = None;
    let mut map_path: Option<PathBuf> = None;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--out" => {
                out = Some(PathBuf::from(args.get(i + 1).cloned().unwrap_or_default()));
                i += 2;
            }
            "--min-salience" => {
                min_salience = args.get(i + 1).and_then(|s| s.parse().ok()).unwrap_or(0.0);
                i += 2;
            }
            "--include-concepts" => {
                include_concepts = true;
                i += 1;
            }
            "--apply-to" => {
                apply_to = Some(PathBuf::from(args.get(i + 1).cloned().unwrap_or_default()));
                i += 2;
            }
            "--map" => {
                map_path = Some(PathBuf::from(args.get(i + 1).cloned().unwrap_or_default()));
                i += 2;
            }
            other if !other.starts_with("--") && corpus_id.is_none() => {
                corpus_id = Some(other.to_string());
                i += 1;
            }
            other => {
                eprintln!("Unknown argument: {other}");
                help::print(&HELP);
                return 1;
            }
        }
    }

    let default_root = dirs::home_dir()
        .map(|h| h.join(".sovereign").join("conversations"))
        .unwrap_or_else(|| PathBuf::from("."));

    // ─── Apply mode ────────────────────────────────────────────────
    if let Some(bank_path) = apply_to {
        let map_path = map_path.unwrap_or_else(|| default_root.join("entity-map.json"));
        return run_apply(&bank_path, &map_path);
    }

    // ─── Candidates mode ───────────────────────────────────────────
    let corpus_id = match corpus_id {
        Some(c) => c,
        None => {
            eprintln!("missing corpus_id argument");
            help::print(&HELP);
            return 1;
        }
    };
    let atoms_path = dirs::home_dir()
        .map(|h| {
            h.join(".sovereign")
                .join("indexes")
                .join(&corpus_id)
                .join("atlas")
                .join("atoms.json")
        })
        .unwrap_or_else(|| PathBuf::from("."));
    let out = out.unwrap_or_else(|| default_root.join("entity-candidates.json"));

    if !atoms_path.exists() {
        eprintln!(
            "No atoms.json found for corpus `{}`.\n\
             Expected at: {}\n\n\
             Atlas enrichment must run before scrub can extract entities. \
             Enable `[enrichment] enabled = true` in the recipe and re-ingest, \
             then re-run this command.",
            corpus_id,
            atoms_path.display(),
        );
        return 2;
    }

    let raw = match std::fs::read_to_string(&atoms_path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("Failed to read {}: {e}", atoms_path.display());
            return 3;
        }
    };
    let parsed: AtomsFile = match serde_json::from_str(&raw) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("Failed to parse {}: {e}", atoms_path.display());
            return 4;
        }
    };

    let candidates = extract_candidates(&parsed.atoms, min_salience, include_concepts);

    if let Some(parent) = out.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            eprintln!("Failed to create {}: {e}", parent.display());
            return 5;
        }
    }
    let json = match serde_json::to_string_pretty(&candidates) {
        Ok(j) => j,
        Err(e) => {
            eprintln!("Serialize error: {e}");
            return 6;
        }
    };
    if let Err(e) = std::fs::write(&out, json) {
        eprintln!("Failed to write {}: {e}", out.display());
        return 7;
    }

    eprintln!(
        "scrub: {} entity candidates written from {} to {}",
        candidates.len(),
        atoms_path.display(),
        out.display(),
    );
    eprintln!(
        "next: review {}, promote real people/orgs into ~/.sovereign/conversations/entity-map.json.",
        out.display()
    );
    0
}

/// Filter Entity atoms whose `entity_type` is person/org-shaped (or
/// concept when `include_concepts`), dedupe on canonical_name (case-
/// insensitive — atlas resolver should have done this but we belt-
/// and-brace), sort descending by salience.
fn extract_candidates(
    atoms: &[RawAtom],
    min_salience: f32,
    include_concepts: bool,
) -> Vec<EntityCandidate> {
    use std::collections::BTreeMap;
    let mut best_by_canonical: BTreeMap<String, EntityCandidate> = BTreeMap::new();

    for atom in atoms {
        if atom.atom_type != "Entity" {
            continue;
        }
        let d = &atom.data;
        let entity_type = d.get("entity_type").and_then(|v| v.as_str()).unwrap_or("");
        let kind_hint = match entity_type {
            "person" | "people" => "person",
            // The conversational + personal domains emit `institution`
            // (companies, agencies, formal bodies) and `initiative`
            // (named projects, programs, ongoing efforts). Both
            // belong to the org-shaped axis for entity-map purposes —
            // they identify third parties whose names need scrubbing
            // before bench artifacts can be committed.
            "organization" | "org" | "institution" => "org",
            "initiative" => "initiative",
            // `work` (books, films, papers, recipes, songs, sermons,
            // internal docs) is a distinct shape from the others —
            // surface it so the user can decide which works to
            // tokenize. Personal manuscripts or unpublished docs may
            // need scrubbing; published references typically don't.
            "work" => "work",
            "concept" if include_concepts => "concept",
            _ => continue,
        };
        let name = d
            .get("canonical_name")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim();
        if name.is_empty() {
            continue;
        }
        let salience = d.get("salience").and_then(|v| v.as_f64()).unwrap_or(0.0) as f32;
        if salience < min_salience {
            continue;
        }
        let description = d
            .get("description")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim()
            .to_string();

        let key = name.to_lowercase();
        let cand = EntityCandidate {
            surface: name.to_string(),
            kind_hint: kind_hint.to_string(),
            salience,
            description,
        };
        best_by_canonical
            .entry(key)
            .and_modify(|e| {
                if cand.salience > e.salience {
                    *e = cand.clone();
                }
            })
            .or_insert(cand);
    }

    let mut out: Vec<EntityCandidate> = best_by_canonical.into_values().collect();
    out.sort_by(|a, b| {
        b.salience
            .partial_cmp(&a.salience)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.surface.cmp(&b.surface))
    });
    out
}

fn run_apply(bank_path: &Path, map_path: &Path) -> i32 {
    let mut map = match EntityMap::load(map_path) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("Failed to load EntityMap from {}: {e}", map_path.display());
            return 8;
        }
    };
    let raw = match std::fs::read_to_string(bank_path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("Failed to read {}: {e}", bank_path.display());
            return 9;
        }
    };
    let bak = bank_path.with_extension("toml.bak");
    if let Err(e) = std::fs::write(&bak, &raw) {
        eprintln!("Failed to write backup {}: {e}", bak.display());
        return 10;
    }
    let scrubbed = scrub_pii(&raw, &mut map);
    if let Err(e) = std::fs::write(bank_path, &scrubbed.text) {
        eprintln!("Failed to write scrubbed bank {}: {e}", bank_path.display());
        return 11;
    }
    eprintln!(
        "scrub: applied EntityMap ({} persons, {} orgs) to {} (backup at {}). Replacements: {:?}",
        map.person_count(),
        map.org_count(),
        bank_path.display(),
        bak.display(),
        scrubbed.replacements,
    );
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mk(atom_type: &str, name: &str, etype: &str, sal: f64) -> RawAtom {
        RawAtom {
            atom_type: atom_type.to_string(),
            data: serde_json::json!({
                "canonical_name": name,
                "entity_type": etype,
                "salience": sal,
                "description": format!("desc for {name}"),
            }),
        }
    }

    #[test]
    fn extracts_persons_and_orgs_excludes_concepts_by_default() {
        let atoms = vec![
            mk("Entity", "Alex Bryan", "person", 0.8),
            mk("Entity", "Acme Corp", "organization", 0.5),
            mk("Entity", "Free Will", "concept", 0.9),
            mk("Claim", "x", "person", 0.9),
        ];
        let out = extract_candidates(&atoms, 0.0, false);
        let names: Vec<&str> = out.iter().map(|c| c.surface.as_str()).collect();
        assert!(names.contains(&"Alex Bryan"));
        assert!(names.contains(&"Acme Corp"));
        assert!(!names.contains(&"Free Will"));
    }

    #[test]
    fn surfaces_institution_and_initiative_entity_types() {
        let atoms = vec![
            mk("Entity", "Federal Reserve", "institution", 0.6),
            mk("Entity", "Q3 Enterprise Push", "initiative", 0.5),
            mk("Entity", "Acme Corp", "organization", 0.4),
        ];
        let out = extract_candidates(&atoms, 0.0, false);
        let by_name: std::collections::BTreeMap<_, _> = out
            .iter()
            .map(|c| (c.surface.clone(), c.kind_hint.clone()))
            .collect();
        assert_eq!(
            by_name.get("Federal Reserve").map(String::as_str),
            Some("org")
        );
        assert_eq!(
            by_name.get("Q3 Enterprise Push").map(String::as_str),
            Some("initiative")
        );
        assert_eq!(by_name.get("Acme Corp").map(String::as_str), Some("org"));
    }

    #[test]
    fn include_concepts_flag_surfaces_concept_atoms() {
        let atoms = vec![
            mk("Entity", "Alex", "person", 0.5),
            mk("Entity", "Compatibilism", "concept", 0.9),
        ];
        let out = extract_candidates(&atoms, 0.0, true);
        let names: Vec<&str> = out.iter().map(|c| c.surface.as_str()).collect();
        assert!(names.contains(&"Compatibilism"));
    }

    #[test]
    fn min_salience_filters_below_threshold() {
        let atoms = vec![
            mk("Entity", "Strong", "person", 0.9),
            mk("Entity", "Weak", "person", 0.1),
        ];
        let out = extract_candidates(&atoms, 0.5, false);
        let names: Vec<&str> = out.iter().map(|c| c.surface.as_str()).collect();
        assert_eq!(names, vec!["Strong"]);
    }

    #[test]
    fn sorted_descending_by_salience() {
        let atoms = vec![
            mk("Entity", "Low", "person", 0.2),
            mk("Entity", "High", "person", 0.9),
            mk("Entity", "Mid", "person", 0.5),
        ];
        let out = extract_candidates(&atoms, 0.0, false);
        let names: Vec<&str> = out.iter().map(|c| c.surface.as_str()).collect();
        assert_eq!(names, vec!["High", "Mid", "Low"]);
    }

    #[test]
    fn dedupes_on_canonical_name_keeping_max_salience() {
        let atoms = vec![
            mk("Entity", "Alex Bryan", "person", 0.3),
            mk("Entity", "alex bryan", "person", 0.7),
        ];
        let out = extract_candidates(&atoms, 0.0, false);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].salience, 0.7);
    }

    #[test]
    fn empty_canonical_name_dropped() {
        let atoms = vec![
            mk("Entity", "", "person", 0.9),
            mk("Entity", "Real", "person", 0.5),
        ];
        let out = extract_candidates(&atoms, 0.0, false);
        let names: Vec<&str> = out.iter().map(|c| c.surface.as_str()).collect();
        assert_eq!(names, vec!["Real"]);
    }
}
