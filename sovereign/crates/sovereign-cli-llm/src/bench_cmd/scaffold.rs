// SPDX-License-Identifier: AGPL-3.0-or-later
//! `sovereign bench scaffold <corpus-id>` — draft a golden TOML
//! from an existing resolved atlas.
//!
//! Reads `~/.sovereign/indexes/<corpus-id>/atlas/atoms.json`, samples
//! N entries per atom kind (typed-axis catalog kinds first, then base
//! kinds), emits a `GoldenSet`-shaped TOML the author can prune /
//! extend / add forbidden entries to. Cuts new-bench authoring from
//! hours of reading the corpus to minutes of pruning the draft.
//!
//! Sampling is deterministic (evenly-spaced across the sorted atom
//! list per kind) so re-running on the same atoms.json produces the
//! same draft — useful for diff-based review.

use std::fs;
use std::path::PathBuf;

use corpus_engine::enrichment::atlas::atoms::{
    AtomEnvelope, Entity, Event, Opposition, Position, Question,
};
use corpus_engine::enrichment::atlas::axis_catalog::{all_axes, AtomKind, TypedAxis};
use corpus_engine::enrichment::atlas::ATLAS_DIRNAME;

use crate::enrich_cmd::paths::index_root;
use sovereign_cli_shared::help::{self, Help, HelpSection};

const HELP: Help = Help {
    command: "sovereign bench scaffold",
    summary: "Draft a golden TOML from an existing resolved atlas — populate; prune; commit.",
    sections: &[
        HelpSection::Usage("sovereign bench scaffold <corpus-id> [--per-axis N] [--output <path>]"),
        HelpSection::Flags(&[
            (
                "--per-axis N",
                "Number of expectations per atom kind. Default: 10.",
            ),
            (
                "--output <path>",
                "Write the draft TOML here. Default: stdout.",
            ),
        ]),
        HelpSection::Examples(&[
            (
                "sovereign bench scaffold obsidian-vault --output /tmp/draft.toml",
                "Sample obsidian-vault atoms.json, emit a draft golden.",
            ),
            (
                "sovereign bench scaffold sep --per-axis 5",
                "Tighter sample for a quick read.",
            ),
        ]),
        HelpSection::Notes(
            "The scaffold encodes what the extractor PRODUCED, not what is correct. \
             Review every entry; tighten name_contains_any to canonical substrings \
             (not whole names); add forbidden_* blocks for known failure modes; fill \
             description_keywords_any from the source text. Treat the draft as a \
             starting point, not a finished golden.",
        ),
    ],
};

const DEFAULT_PER_AXIS: usize = 10;

pub async fn cmd_scaffold(args: &[String]) -> i32 {
    if help::wants_help(args) {
        help::print(&HELP);
        return 0;
    }
    let parsed = match parse_args(args) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("error: {e}");
            help::print(&HELP);
            return 2;
        }
    };

    let atoms_path = index_root(&parsed.corpus_id)
        .join(ATLAS_DIRNAME)
        .join("atoms.json");
    if !atoms_path.exists() {
        eprintln!(
            "error: {} not found. Build the atlas first: `sovereign enrich build {}`.",
            atoms_path.display(),
            parsed.corpus_id
        );
        return 1;
    }

    let bytes = match fs::read_to_string(&atoms_path) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("error: read {}: {e}", atoms_path.display());
            return 1;
        }
    };
    let parsed_atoms: AtomsFileLite = match serde_json::from_str(&bytes) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("error: parse {}: {e}", atoms_path.display());
            return 1;
        }
    };

    let draft = scaffold_draft(&parsed.corpus_id, &parsed_atoms.atoms, parsed.per_axis);
    let rendered = render_toml(&draft);

    match parsed.output.as_deref() {
        Some(path) => {
            if let Some(parent) = path.parent() {
                let _ = fs::create_dir_all(parent);
            }
            if let Err(e) = fs::write(path, &rendered) {
                eprintln!("error: write {}: {e}", path.display());
                return 1;
            }
            eprintln!("wrote scaffold to {}", path.display());
            eprintln!(
                "review + prune + add forbidden_* blocks before running `sovereign bench all`",
            );
            0
        }
        None => {
            print!("{rendered}");
            0
        }
    }
}

// ── Lite atoms.json shape ────────────────────────────────────────
//
// Sidesteps importing the full AtomsFile (which carries
// SCHEMA_VERSION + manifest fields the scaffolder doesn't need).

#[derive(Debug, serde::Deserialize)]
struct AtomsFileLite {
    atoms: Vec<AtomEnvelope>,
}

// ── Draft structure ──────────────────────────────────────────────

#[derive(Debug, Default)]
struct DraftGolden {
    corpus_id: String,
    // Catalog axes (typed)
    axes: Vec<DraftAxis>,
    // Base atom kinds
    persons: Vec<DraftEntity>,
    concepts: Vec<DraftEntity>,
    works: Vec<DraftEntity>,
    events: Vec<DraftEvent>,
    questions: Vec<DraftQuestion>,
}

#[derive(Debug)]
struct DraftAxis {
    key: &'static str, // catalog axis key
    entries: Vec<DraftAxisEntry>,
}

#[derive(Debug)]
struct DraftAxisEntry {
    // Different axes use different field names in the golden TOML;
    // this carries the union so the renderer can pick.
    name: Option<String>,   // mechanism / named_position / concession / evidence
    stance: Option<String>, // named_position
    kind: Option<String>,   // evidence
    left: Option<String>,   // opposition
    right: Option<String>,  // opposition
    axis_label: Option<String>, // opposition
}

#[derive(Debug)]
struct DraftEntity {
    canonical_name: String,
    description_hint: String,
}

#[derive(Debug)]
struct DraftEvent {
    description: String,
}

#[derive(Debug)]
struct DraftQuestion {
    content: String,
}

// ── Sampling ─────────────────────────────────────────────────────

fn scaffold_draft(corpus_id: &str, atoms: &[AtomEnvelope], per_axis: usize) -> DraftGolden {
    let mut draft = DraftGolden {
        corpus_id: corpus_id.to_string(),
        ..Default::default()
    };

    // Typed-axis sampling (catalog-driven)
    for axis in all_axes() {
        let entries = sample_axis(atoms, axis, per_axis);
        if !entries.is_empty() {
            draft.axes.push(DraftAxis {
                key: axis.key,
                entries,
            });
        }
    }

    // Base atom-kind sampling. Filter by entity_type for the three
    // Entity sub-kinds (Person / Concept / Work); everything else
    // pulls from its own envelope variant.
    draft.persons = sample_entities(
        atoms,
        |e| {
            matches!(
                e.entity_type,
                corpus_engine::enrichment::pipeline::atlas::EntityType::Person
            )
        },
        per_axis,
    );
    draft.concepts = sample_entities(
        atoms,
        |e| {
            // Skip qualified concepts (mechanism etc.) — those went into
            // their axis lane. Keep base / unqualified concepts here.
            matches!(
                e.entity_type,
                corpus_engine::enrichment::pipeline::atlas::EntityType::Concept
            ) && e.concept_kind.is_none()
        },
        per_axis,
    );
    draft.works = sample_entities(
        atoms,
        |e| {
            matches!(
                e.entity_type,
                corpus_engine::enrichment::pipeline::atlas::EntityType::Work
            )
        },
        per_axis,
    );

    draft.events = sample_events(atoms, per_axis);
    draft.questions = sample_questions(atoms, per_axis);

    draft
}

fn sample_axis(atoms: &[AtomEnvelope], axis: &TypedAxis, n: usize) -> Vec<DraftAxisEntry> {
    let candidates: Vec<DraftAxisEntry> = match axis.atom_kind {
        AtomKind::EntityWithConceptKind(tag) => atoms
            .iter()
            .filter_map(|a| match a {
                AtomEnvelope::Entity(e) if e.concept_kind.as_deref() == Some(tag) => {
                    Some(DraftAxisEntry {
                        name: Some(e.canonical_name.clone()),
                        stance: None,
                        kind: None,
                        left: None,
                        right: None,
                        axis_label: None,
                    })
                }
                _ => None,
            })
            .collect(),
        AtomKind::ClaimWithKind(tag) => atoms
            .iter()
            .filter_map(|a| match a {
                AtomEnvelope::Claim(c) if c.claim_kind.as_deref() == Some(tag) => {
                    Some(DraftAxisEntry {
                        // Claim has no `name` — use a content
                        // prefix as the name needle. truncate_needle
                        // omits the U+2026 ellipsis so substring
                        // matching against the full content still
                        // works at scoring time.
                        name: Some(truncate_needle(&c.content, 60)),
                        stance: None,
                        kind: c
                            .evidence_kind
                            .clone()
                            .or_else(|| c.concession_outcome.clone()),
                        left: None,
                        right: None,
                        axis_label: None,
                    })
                }
                _ => None,
            })
            .collect(),
        AtomKind::Position => atoms
            .iter()
            .filter_map(|a| match a {
                AtomEnvelope::Position(p) => Some(position_to_entry(p)),
                _ => None,
            })
            .collect(),
        AtomKind::Opposition => atoms
            .iter()
            .filter_map(|a| match a {
                AtomEnvelope::Opposition(o) => Some(opposition_to_entry(o)),
                _ => None,
            })
            .collect(),
        _ => Vec::new(), // base kinds handled separately
    };

    evenly_spaced(candidates, n)
}

fn position_to_entry(p: &Position) -> DraftAxisEntry {
    DraftAxisEntry {
        name: Some(p.canonical_name.clone()),
        stance: Some(p.stance.clone()),
        kind: None,
        left: None,
        right: None,
        axis_label: None,
    }
}

fn opposition_to_entry(o: &Opposition) -> DraftAxisEntry {
    DraftAxisEntry {
        name: None,
        stance: None,
        kind: None,
        left: Some(o.left_label.clone()),
        right: Some(o.right_label.clone()),
        axis_label: Some(o.axis.clone()),
    }
}

fn sample_entities<F>(atoms: &[AtomEnvelope], filter: F, n: usize) -> Vec<DraftEntity>
where
    F: Fn(&Entity) -> bool,
{
    let mut cands: Vec<DraftEntity> = atoms
        .iter()
        .filter_map(|a| match a {
            AtomEnvelope::Entity(e) if filter(e) => Some(DraftEntity {
                canonical_name: e.canonical_name.clone(),
                description_hint: truncate(&e.description, 80),
            }),
            _ => None,
        })
        .collect();
    cands.sort_by(|a, b| a.canonical_name.cmp(&b.canonical_name));
    evenly_spaced(cands, n)
}

fn sample_events(atoms: &[AtomEnvelope], n: usize) -> Vec<DraftEvent> {
    let mut cands: Vec<DraftEvent> = atoms
        .iter()
        .filter_map(|a| match a {
            AtomEnvelope::Event(e) => Some(DraftEvent {
                description: truncate(&e.description, 80),
            }),
            _ => None,
        })
        .collect();
    cands.sort_by(|a, b| a.description.cmp(&b.description));
    evenly_spaced(cands, n)
}

fn sample_questions(atoms: &[AtomEnvelope], n: usize) -> Vec<DraftQuestion> {
    let mut cands: Vec<DraftQuestion> = atoms
        .iter()
        .filter_map(|a| match a {
            AtomEnvelope::Question(q) => Some(DraftQuestion {
                content: truncate(&q.content, 80),
            }),
            _ => None,
        })
        .collect();
    cands.sort_by(|a, b| a.content.cmp(&b.content));
    evenly_spaced(cands, n)
}

/// Take up to N items, evenly spaced across the input slice.
/// Deterministic: same input → same output. Spreads samples across
/// the alphabetical distribution so the author sees a representative
/// slice, not just the first N.
fn evenly_spaced<T>(mut items: Vec<T>, n: usize) -> Vec<T> {
    if items.len() <= n {
        return items;
    }
    let stride = items.len() as f32 / n as f32;
    let mut picked = Vec::with_capacity(n);
    let mut taken_indexes = std::collections::HashSet::new();
    for i in 0..n {
        let idx = (i as f32 * stride) as usize;
        let idx = idx.min(items.len() - 1);
        if taken_indexes.insert(idx) {
            picked.push(idx);
        }
    }
    picked.sort_unstable_by(|a, b| b.cmp(a)); // remove from end first
    let mut out: Vec<T> = picked
        .into_iter()
        .map(|idx| items.swap_remove(idx))
        .collect();
    out.reverse();
    out
}

fn truncate(s: &str, n: usize) -> String {
    let trimmed = s.trim();
    if trimmed.chars().count() <= n {
        return trimmed.to_string();
    }
    trimmed.chars().take(n).collect::<String>() + "…"
}

/// Truncate without the ellipsis suffix — used for golden TOML
/// needles where the truncated string must remain a literal
/// substring of the source atom's full content. `…` is U+2026 and
/// will not appear in the atom's content, so adding it breaks the
/// substring match the scorer relies on.
fn truncate_needle(s: &str, n: usize) -> String {
    s.trim().chars().take(n).collect()
}

// ── TOML rendering ───────────────────────────────────────────────

fn render_toml(d: &DraftGolden) -> String {
    let mut out = String::new();
    out.push_str("# Scaffolded golden — REVIEW before use.\n");
    out.push_str("# Entries below were sampled from atoms.json. They encode what the\n");
    out.push_str("# extractor PRODUCED, not what is correct. Tighten name_contains_any\n");
    out.push_str("# to canonical substrings, fill description_keywords_any, add\n");
    out.push_str("# forbidden_* blocks for known failure modes.\n\n");

    out.push_str("[meta]\n");
    out.push_str(&format!("template = \"scaffold-{}\"\n", d.corpus_id));
    out.push_str(&format!("corpus_id = \"{}\"\n", d.corpus_id));
    out.push_str(&format!(
        "description = \"Auto-scaffolded golden draft for `{}`. Review + prune before committing.\"\n\n",
        d.corpus_id
    ));

    // Catalog axes
    for axis in &d.axes {
        out.push_str(&format!("# ── {} axis ──\n\n", axis.key));
        for entry in &axis.entries {
            out.push_str(&format!("[[expected_{}_atoms]]\n", axis.key));
            match axis.key {
                "opposition" => {
                    if let Some(l) = &entry.left {
                        out.push_str(&format!("left_contains_any = [{}]\n", quote(l)));
                    }
                    if let Some(r) = &entry.right {
                        out.push_str(&format!("right_contains_any = [{}]\n", quote(r)));
                    }
                    if let Some(ax) = &entry.axis_label {
                        out.push_str(&format!(
                            "axis_contains_any = [{}]\n",
                            quote(&truncate(ax, 50))
                        ));
                    }
                }
                "evidence" | "concession" => {
                    if let Some(n) = &entry.name {
                        let field = if axis.key == "evidence" {
                            "label_contains_any"
                        } else {
                            "content_contains_any"
                        };
                        out.push_str(&format!("{field} = [{}]\n", quote(n)));
                    }
                    if let Some(k) = &entry.kind {
                        let field = if axis.key == "evidence" {
                            "kind"
                        } else {
                            "outcome"
                        };
                        out.push_str(&format!("{field} = \"{k}\"\n"));
                    }
                }
                "named_position" => {
                    if let Some(n) = &entry.name {
                        out.push_str(&format!("name_contains_any = [{}]\n", quote(n)));
                    }
                    if let Some(s) = &entry.stance {
                        out.push_str(&format!("stance = \"{s}\"\n"));
                    }
                }
                _ => {
                    // mechanism + future catalog axes use name_contains_any
                    if let Some(n) = &entry.name {
                        out.push_str(&format!("name_contains_any = [{}]\n", quote(n)));
                    }
                }
            }
            out.push_str("# description_keywords_any = []  # author fills from source\n");
            out.push_str("note = \"scaffolded — verify\"\n\n");
        }
    }

    // Base atom kinds
    if !d.persons.is_empty() {
        out.push_str("# ── person atoms ──\n\n");
        for e in &d.persons {
            render_entity_block(&mut out, "person", e);
        }
    }
    if !d.concepts.is_empty() {
        out.push_str("# ── concept atoms ──\n\n");
        for e in &d.concepts {
            render_entity_block(&mut out, "concept", e);
        }
    }
    if !d.works.is_empty() {
        out.push_str("# ── work atoms ──\n\n");
        for e in &d.works {
            render_entity_block(&mut out, "work", e);
        }
    }
    if !d.events.is_empty() {
        out.push_str("# ── event atoms ──\n\n");
        for e in &d.events {
            out.push_str("[[expected_event_atoms]]\n");
            out.push_str(&format!(
                "description_contains_any = [{}]\n",
                quote(&truncate(&e.description, 50))
            ));
            out.push_str("note = \"scaffolded — verify\"\n\n");
        }
    }
    if !d.questions.is_empty() {
        out.push_str("# ── question atoms ──\n\n");
        for q in &d.questions {
            out.push_str("[[expected_question_atoms]]\n");
            out.push_str(&format!(
                "content_contains_any = [{}]\n",
                quote(&truncate(&q.content, 50))
            ));
            out.push_str("note = \"scaffolded — verify\"\n\n");
        }
    }

    out
}

fn render_entity_block(out: &mut String, kind: &str, e: &DraftEntity) {
    out.push_str(&format!("[[expected_{kind}_atoms]]\n"));
    out.push_str(&format!(
        "canonical_name_contains_any = [{}]\n",
        quote(&e.canonical_name)
    ));
    if !e.description_hint.is_empty() {
        out.push_str(&format!(
            "# description hint (from source): {}\n",
            e.description_hint
        ));
    }
    out.push_str("# description_keywords_any = []\n");
    out.push_str("note = \"scaffolded — verify\"\n\n");
}

fn quote(s: &str) -> String {
    // TOML basic-string quoting: escape backslashes + quotes.
    let escaped = s.replace('\\', "\\\\").replace('"', "\\\"");
    format!("\"{escaped}\"")
}

// ── Args ─────────────────────────────────────────────────────────

#[derive(Debug)]
struct ParsedScaffold {
    corpus_id: String,
    per_axis: usize,
    output: Option<PathBuf>,
}

fn parse_args(args: &[String]) -> Result<ParsedScaffold, String> {
    let mut corpus_id: Option<String> = None;
    let mut per_axis = DEFAULT_PER_AXIS;
    let mut output: Option<PathBuf> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--per-axis" => {
                let v = args.get(i + 1).ok_or("--per-axis requires a value")?;
                per_axis = v.parse::<usize>().map_err(|e| format!("--per-axis: {e}"))?;
                i += 2;
            }
            "--output" => {
                let v = args.get(i + 1).ok_or("--output requires a path")?;
                output = Some(PathBuf::from(v));
                i += 2;
            }
            other if other.starts_with("--") => {
                return Err(format!("unknown flag: {other}"));
            }
            other => {
                if corpus_id.is_some() {
                    return Err(format!("unexpected positional: {other}"));
                }
                corpus_id = Some(other.to_string());
                i += 1;
            }
        }
    }
    Ok(ParsedScaffold {
        corpus_id: corpus_id.ok_or_else(|| "missing <corpus-id>".to_string())?,
        per_axis,
        output,
    })
}

// Reference imports kept so unused-import lints stay quiet when
// these types are referenced only via match-arms below.
#[allow(dead_code)]
fn _ref_imports(_: &Event, _: &Question) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn evenly_spaced_returns_all_when_under_n() {
        let v = vec![1, 2, 3];
        assert_eq!(evenly_spaced(v, 10), vec![1, 2, 3]);
    }

    #[test]
    fn evenly_spaced_spans_distribution() {
        let v: Vec<i32> = (0..100).collect();
        let s = evenly_spaced(v, 10);
        assert_eq!(s.len(), 10);
        // First sample is 0; last sample is near 90.
        assert_eq!(s.first().copied(), Some(0));
        assert!(s.last().copied().unwrap_or(0) >= 80);
    }

    #[test]
    fn truncate_preserves_short_strings() {
        assert_eq!(truncate("hi", 10), "hi");
    }

    #[test]
    fn truncate_adds_ellipsis() {
        let s = truncate("hello world", 5);
        assert!(s.ends_with('…'));
    }

    #[test]
    fn parse_args_corpus_only() {
        let p = parse_args(&["obsidian-vault".into()]).unwrap();
        assert_eq!(p.corpus_id, "obsidian-vault");
        assert_eq!(p.per_axis, DEFAULT_PER_AXIS);
        assert!(p.output.is_none());
    }

    #[test]
    fn parse_args_full() {
        let p = parse_args(&[
            "obs".into(),
            "--per-axis".into(),
            "5".into(),
            "--output".into(),
            "/tmp/x.toml".into(),
        ])
        .unwrap();
        assert_eq!(p.corpus_id, "obs");
        assert_eq!(p.per_axis, 5);
        assert_eq!(p.output, Some(PathBuf::from("/tmp/x.toml")));
    }
}
