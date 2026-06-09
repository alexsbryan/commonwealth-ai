// SPDX-License-Identifier: AGPL-3.0-or-later
//! `sovereign awareness eval` — score the current atlas against a
//! golden set.
//!
//! Two ways to load the golden set (mutually exclusive):
//!
//!   - `--golden <path>` — JSONL file with one record per
//!     conversation; each record carries `expected_entities` plus
//!     optional `expected_suggestions` arrays.
//!
//!   - `--from-template <name>` — read the same metadata that's
//!     baked into the built-in template (`consulting`, `startup`,
//!     `team-lead`).
//!
//! `--report <path>` writes the structured comparison as JSON for
//! tracking quality across prompt iterations.
//!
//! Phase 4 ships entity scoring (precision / recall / F1) plus
//! merge-accuracy heuristics. Suggestion scoring is exposed as a
//! library function in `golden.rs` and is called from
//! `scenario.rs::suggestion_quality`; running it from `eval` would
//! require driving a per-conversation `suggest` replay, which is
//! the scenario subcommand's job — `eval` stays focused on what's
//! already in the atlas.

use std::path::PathBuf;

use corpus_engine::enrichment::atlas::atoms::AtomEnvelope;
use corpus_engine::enrichment::atlas::writer::read_atlas_atoms;
use corpus_engine::enrichment::pipeline::atlas::EntityType;
use serde_json::json;

use super::args::{get_flag, has_flag, split_args};
use super::golden::{score_entities, EntityScore, ExpectedEntity, GoldenSet};
use super::render::display_path;
use super::store_open::{atlas_dir_for, sovereign_root};
use super::templates::load_builtin;

const RELATIONAL_VIEWS: &[&str] = &["personal-knowledge", "conversation-history"];

pub(super) async fn cmd_eval(args: &[String]) -> i32 {
    let (positional, flags) = split_args(args);

    // Golden set source — flag has priority; positional is treated
    // as a JSONL path for ergonomics.
    let golden = match resolve_golden(&flags, positional.first()) {
        Ok(g) => g,
        Err(e) => {
            eprintln!("awareness eval: {e}");
            return 2;
        }
    };

    let report_path: Option<PathBuf> = get_flag(&flags, "report")
        .filter(|s| !s.is_empty())
        .map(PathBuf::from);
    let json_only = has_flag(&flags, "json");

    // Read both atlases; collapse to a list of (kind, name) pairs.
    let root = sovereign_root(&flags);
    let mut extracted_names: Vec<String> = Vec::new();
    let mut atlases_seen = 0usize;
    for view_id in RELATIONAL_VIEWS {
        let atlas_dir = atlas_dir_for(&root, view_id);
        if !atlas_dir.exists() {
            continue;
        }
        atlases_seen += 1;
        let atoms = match read_atlas_atoms(&atlas_dir) {
            Ok(f) => f,
            Err(e) => {
                eprintln!(
                    "awareness eval: failed to read {}/atoms.json: {e}",
                    display_path(&atlas_dir)
                );
                return 1;
            }
        };
        for atom in atoms.atoms {
            if let AtomEnvelope::Entity(e) = atom {
                if classify_kind(&e.entity_type).is_some() {
                    extracted_names.push(e.canonical_name);
                }
            }
        }
    }
    if atlases_seen == 0 {
        eprintln!(
            "awareness eval: no atlases found at {}/indexes/* — \
             run `awareness extract` first",
            display_path(&root)
        );
        return 1;
    }

    // ── Score per kind plus combined ─────────────────────────────
    let combined = score_entities(&golden.expected_entities, &extracted_names);
    let person = score_kind(&golden.expected_entities, &extracted_names, "person");
    let org = score_kind(&golden.expected_entities, &extracted_names, "organization");
    let init = score_kind(&golden.expected_entities, &extracted_names, "initiative");

    if !json_only {
        print_text_report(&golden, &combined, &person, &org, &init);
    }

    if let Some(path) = report_path {
        let report = json!({
            "atlases_present": atlases_seen,
            "extracted_total": extracted_names.len(),
            "expected_total": golden.expected_entities.len(),
            "by_kind": {
                "person": render_score(&person),
                "organization": render_score(&org),
                "initiative": render_score(&init),
            },
            "combined": render_score(&combined),
        });
        if let Err(e) = std::fs::write(
            &path,
            serde_json::to_string_pretty(&report).unwrap_or_default(),
        ) {
            eprintln!(
                "awareness eval: failed to write report {}: {e}",
                display_path(&path)
            );
            return 1;
        }
        if !json_only {
            println!();
            println!("Wrote report to {}", display_path(&path));
        }
    }

    if json_only {
        let report = json!({
            "atlases_present": atlases_seen,
            "extracted_total": extracted_names.len(),
            "expected_total": golden.expected_entities.len(),
            "by_kind": {
                "person": render_score(&person),
                "organization": render_score(&org),
                "initiative": render_score(&init),
            },
            "combined": render_score(&combined),
        });
        println!(
            "{}",
            serde_json::to_string_pretty(&report).unwrap_or_default()
        );
    }

    // Exit 0 on healthy F1 (>= 0.5), 1 otherwise — useful for
    // scripted workflows. The threshold is informative; the developer
    // looks at the false-positive / false-negative lists for signal.
    if combined.f1() >= 0.5 {
        0
    } else {
        1
    }
}

/// Pick the golden set source. Errors when no source is given.
pub(super) fn resolve_golden(
    flags: &[(String, String)],
    positional: Option<&String>,
) -> Result<GoldenSet, String> {
    if let Some(name) = get_flag(flags, "from-template").filter(|s| !s.is_empty()) {
        return Ok(GoldenSet::from_template(&load_builtin(&name)?));
    }
    if let Some(p) = get_flag(flags, "golden").filter(|s| !s.is_empty()) {
        return load_jsonl(&p);
    }
    if let Some(p) = positional {
        return load_jsonl(p);
    }
    Err("pass --from-template <name> or --golden <path-to-jsonl> (or a positional path)".into())
}

fn load_jsonl(path: &str) -> Result<GoldenSet, String> {
    let body = std::fs::read_to_string(path).map_err(|e| format!("read {path}: {e}"))?;
    GoldenSet::from_jsonl(&body)
}

fn classify_kind(t: &EntityType) -> Option<&'static str> {
    match t {
        EntityType::Person => Some("person"),
        EntityType::Institution => Some("organization"),
        EntityType::Initiative => Some("initiative"),
        _ => None,
    }
}

/// Filter the expected list to a single kind and intersect the
/// extracted list against the same kind via the atlas (only the
/// kind-aware caller knows which entities belong to which bucket;
/// we approximate by matching extracted names to the expected
/// entries that share the kind label). Concretely: a "person"
/// expected name is matched only against extracted names — kind is
/// not preserved on the simple `Vec<String>` we pass in. So this
/// function actually computes: "of the expected entities of kind X,
/// how many appear (by name) in the extracted list overall?" That's
/// what we want — extraction either picks up a person by their
/// name or it doesn't, and we don't penalise extracting the right
/// name with the wrong kind in this simple report.
fn score_kind(expected: &[ExpectedEntity], extracted_names: &[String], kind: &str) -> EntityScore {
    let kind_expected: Vec<ExpectedEntity> = expected
        .iter()
        .filter(|e| e.kind == kind)
        .cloned()
        .collect();
    score_entities(&kind_expected, extracted_names)
}

fn print_text_report(
    golden: &GoldenSet,
    combined: &EntityScore,
    person: &EntityScore,
    org: &EntityScore,
    init: &EntityScore,
) {
    println!("Evaluation against golden set:");
    println!(
        "  Expected: {} entities ({} declared in source)",
        combined.expected,
        golden.expected_entities.len()
    );
    println!("  Extracted: {} entities", combined.extracted);
    println!();
    println!("Combined entity match:");
    print_score_block(combined);
    println!();
    println!("Per-kind:");
    println!("  person:        ");
    print_score_block_indented(person);
    println!("  organization:  ");
    print_score_block_indented(org);
    println!("  initiative:    ");
    print_score_block_indented(init);

    if !combined.false_positives.is_empty() {
        println!();
        println!(
            "False positives ({}): {}",
            combined.false_positives.len(),
            combined.false_positives.join(", ")
        );
    }
    if !combined.false_negatives.is_empty() {
        println!();
        println!(
            "False negatives ({}): {}",
            combined.false_negatives.len(),
            combined.false_negatives.join(", ")
        );
    }

    println!();
    println!("Overall: {}", verdict(combined.f1()));
}

fn print_score_block(s: &EntityScore) {
    println!("  Precision: {:.2}", s.precision());
    println!("  Recall:    {:.2}", s.recall());
    println!("  F1:        {:.2}", s.f1());
    println!("  Matched:   {}/{}", s.matched, s.expected);
}

fn print_score_block_indented(s: &EntityScore) {
    println!(
        "    P {:.2}  R {:.2}  F1 {:.2}  ({}/{} matched)",
        s.precision(),
        s.recall(),
        s.f1(),
        s.matched,
        s.expected
    );
}

fn verdict(f1: f64) -> &'static str {
    if f1 >= 0.85 {
        "GOOD (F1 ≥ 0.85)"
    } else if f1 >= 0.6 {
        "OK (F1 in [0.6, 0.85))"
    } else if f1 >= 0.3 {
        "NEEDS WORK (F1 in [0.3, 0.6))"
    } else {
        "FAILING (F1 < 0.3)"
    }
}

fn render_score(s: &EntityScore) -> serde_json::Value {
    json!({
        "precision": s.precision(),
        "recall": s.recall(),
        "f1": s.f1(),
        "matched": s.matched,
        "expected": s.expected,
        "extracted": s.extracted,
        "false_positives": s.false_positives,
        "false_negatives": s.false_negatives,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verdict_buckets_are_in_order() {
        assert_eq!(verdict(0.95), "GOOD (F1 ≥ 0.85)");
        assert_eq!(verdict(0.7), "OK (F1 in [0.6, 0.85))");
        assert_eq!(verdict(0.4), "NEEDS WORK (F1 in [0.3, 0.6))");
        assert_eq!(verdict(0.1), "FAILING (F1 < 0.3)");
    }

    #[test]
    fn classify_kind_filters_relational() {
        assert_eq!(classify_kind(&EntityType::Person), Some("person"));
        assert_eq!(
            classify_kind(&EntityType::Institution),
            Some("organization")
        );
        assert_eq!(classify_kind(&EntityType::Initiative), Some("initiative"));
        assert_eq!(classify_kind(&EntityType::Concept), None);
    }
}
