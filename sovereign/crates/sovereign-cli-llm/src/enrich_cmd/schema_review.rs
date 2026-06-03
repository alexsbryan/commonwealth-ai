//! `sovereign enrich schema-report` and `schema-review` —
//! Phase C Step 9 diagnostic drivers.
//!
//! **`schema-report <corpus>`** computes the §12 schema
//! validation report for one corpus on demand (from the resolved
//! atlas files), writes it to `atlas/schema_validation.json`, and
//! prints the §12.4 diagnostic table.
//!
//! **`schema-review <corpus-a> <corpus-b>...`** runs the same
//! computation across N corpora and surfaces gap signatures
//! present in ≥ 2 corpora as **schema-revision candidates**
//! (with a recommendation per gap kind); signatures present in
//! exactly one corpus surface as **prompt-tuning candidates**.
//! Per spec §12.5: schema revisions should only be driven by
//! systematic gaps, not idiosyncratic per-corpus artefacts.

use std::path::{Path, PathBuf};

use corpus_engine::enrichment::atlas::{
    build_schema_validation_report, compare_across_corpora, count_open_questions,
    count_transitions_without_trigger, count_ungrounded_claims, read_atlas_atoms,
    read_atlas_cross_corpus_edges, read_atlas_edges, AtomEnvelope, SchemaValidationInput,
    SchemaValidationReport, ATLAS_DIRNAME,
};

use super::config::EnrichConfig;
use super::paths;
use crate::util::help::{self, Help, HelpSection};

// ── schema-report ────────────────────────────────────────────

const REPORT_HELP: Help = Help {
    command: "sovereign enrich schema-report",
    summary: "Compute + print the §12 schema validation report for one corpus.",
    sections: &[
        HelpSection::Usage("sovereign enrich schema-report <corpus-id> [--json]"),
        HelpSection::Flags(&[(
            "--json",
            "Emit the SchemaValidationReport as JSON instead of the human-readable table.",
        )]),
        HelpSection::Examples(&[(
            "sovereign enrich schema-report brothers_karamazov",
            "Print the §12.4 diagnostic table: coverage / depth / confidence / orphans / gaps.",
        )]),
        HelpSection::Notes(
            "Requires a resolved atlas (run `sovereign enrich atlas-resolve <corpus> \
             --phase all` first). The report is computed on demand — retrofitting \
             incremental writes into each phase is a follow-up. Also writes \
             `atlas/schema_validation.json` alongside the other atlas files.",
        ),
    ],
};

pub async fn cmd_schema_report(args: &[String]) -> i32 {
    if help::wants_help(args) {
        help::print(&REPORT_HELP);
        return 0;
    }
    let parsed = match parse_report_args(args) {
        Ok(p) => p,
        Err(msg) => {
            eprintln!("error: {msg}");
            eprintln!();
            help::print(&REPORT_HELP);
            return 2;
        }
    };
    let cfg = match EnrichConfig::require(&parsed.corpus_id) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("error: loading enrichment config: {e}");
            return 1;
        }
    };

    let report = match compute_report(&cfg.corpus_id) {
        Ok(r) => r,
        Err(code) => return code,
    };

    // Write the JSON file alongside the other atlas artifacts so
    // downstream consumers (CI, dashboards) can pick it up.
    let atlas_dir = atlas_dir_for(&cfg.corpus_id);
    let out_path = atlas_dir.join("schema_validation.json");
    if let Err(e) = write_atomic(&out_path, &report) {
        eprintln!("warning: writing {}: {e}", out_path.display());
        // Non-fatal — the table still prints.
    }

    if parsed.as_json {
        match serde_json::to_string_pretty(&report) {
            Ok(s) => println!("{s}"),
            Err(e) => {
                eprintln!("error: serialising report: {e}");
                return 1;
            }
        }
    } else {
        print_human_report(&report);
        println!();
        println!("  ✓ wrote {}", out_path.display());
    }
    0
}

#[derive(Debug)]
struct ParsedReport {
    corpus_id: String,
    as_json: bool,
}

fn parse_report_args(args: &[String]) -> Result<ParsedReport, String> {
    let mut corpus_id: Option<String> = None;
    let mut as_json = false;
    for arg in args {
        match arg.as_str() {
            "--json" => as_json = true,
            other if other.starts_with("--") => {
                return Err(format!("unknown flag: {other}"));
            }
            other => {
                if corpus_id.is_some() {
                    return Err(format!("unexpected positional argument: {other}"));
                }
                corpus_id = Some(other.to_string());
            }
        }
    }
    let corpus_id = corpus_id.ok_or_else(|| "missing <corpus-id>".to_string())?;
    Ok(ParsedReport { corpus_id, as_json })
}

// ── schema-review ────────────────────────────────────────────

const REVIEW_HELP: Help = Help {
    command: "sovereign enrich schema-review",
    summary: "Compare schema validation reports across N corpora; flag systematic gaps.",
    sections: &[
        HelpSection::Usage("sovereign enrich schema-review <corpus-a> <corpus-b> [<corpus-c> ...]"),
        HelpSection::Examples(&[(
            "sovereign enrich schema-review brothers_karamazov compatibilism",
            "Compute both reports; flag gaps present in both as schema-revision candidates.",
        )]),
        HelpSection::Notes(
            "Per spec §12.5: a gap present in ≥ 2 corpora warrants schema revision; \
             a gap present in exactly one warrants prompt tuning. Each corpus must have \
             a resolved atlas.",
        ),
    ],
};

pub async fn cmd_schema_review(args: &[String]) -> i32 {
    if help::wants_help(args) {
        help::print(&REVIEW_HELP);
        return 0;
    }
    let parsed = match parse_review_args(args) {
        Ok(p) => p,
        Err(msg) => {
            eprintln!("error: {msg}");
            eprintln!();
            help::print(&REVIEW_HELP);
            return 2;
        }
    };

    let mut reports = Vec::new();
    for corpus_id in &parsed.corpora {
        let cfg = match EnrichConfig::require(corpus_id) {
            Ok(c) => c,
            Err(e) => {
                eprintln!("error: loading config for `{corpus_id}`: {e}");
                return 1;
            }
        };
        let report = match compute_report(&cfg.corpus_id) {
            Ok(r) => r,
            Err(code) => return code,
        };
        reports.push(report);
    }

    let comparison = compare_across_corpora(&reports);

    println!(
        "=== Schema review across {} corpora ===",
        comparison.corpora.len()
    );
    for c in &comparison.corpora {
        println!("  · {c}");
    }
    println!();

    if comparison.convergent_gaps.is_empty() {
        println!("  No convergent gaps — no schema revision candidates.");
    } else {
        println!("  Convergent gaps (schema revision candidates — present in ≥ 2 corpora):");
        for g in &comparison.convergent_gaps {
            println!();
            println!("  [{}]", g.signature);
            println!("    present_in:     {}", g.present_in.join(", "));
            println!("    recommendation: {}", g.recommendation);
        }
    }
    println!();

    if comparison.idiosyncratic_gaps.is_empty() {
        println!("  No idiosyncratic gaps — nothing single-corpus to tune.");
    } else {
        println!(
            "  Idiosyncratic gaps (prompt-tuning candidates — present in exactly one \
             corpus):"
        );
        for g in &comparison.idiosyncratic_gaps {
            println!("    · [{}] on {}", g.signature, g.present_in);
        }
    }

    0
}

#[derive(Debug)]
struct ParsedReview {
    corpora: Vec<String>,
}

fn parse_review_args(args: &[String]) -> Result<ParsedReview, String> {
    let mut corpora = Vec::new();
    for arg in args {
        match arg.as_str() {
            other if other.starts_with("--") => {
                return Err(format!("unknown flag: {other}"));
            }
            other => corpora.push(other.to_string()),
        }
    }
    if corpora.len() < 2 {
        return Err(format!(
            "expected at least 2 corpus ids, got {}",
            corpora.len()
        ));
    }
    Ok(ParsedReview { corpora })
}

// ── Shared compute path ──────────────────────────────────────

/// Load the atlas + compute the report. Returns the exit code
/// to propagate if anything fails.
fn compute_report(corpus_id: &str) -> Result<SchemaValidationReport, i32> {
    let atlas_dir = atlas_dir_for(corpus_id);
    let atoms = match read_atlas_atoms(&atlas_dir) {
        Ok(a) => a,
        Err(e) => {
            eprintln!(
                "error: reading {}/atoms.json: {e}. Run `sovereign enrich atlas-resolve \
                 {corpus_id} --phase all` first.",
                atlas_dir.display()
            );
            return Err(1);
        }
    };
    let edges = match read_atlas_edges(&atlas_dir) {
        Ok(e) => e,
        Err(err) => {
            eprintln!(
                "error: reading {}/edges.json: {err}. Run `sovereign enrich atlas-resolve \
                 {corpus_id} --phase all` first.",
                atlas_dir.display()
            );
            return Err(1);
        }
    };
    // Cross-corpus is optional — missing is "not run" rather than "error".
    let cross_corpus = read_atlas_cross_corpus_edges(&atlas_dir).ok();

    // Partition atoms so we can feed the deterministic-gap counters.
    let mut claims = Vec::new();
    let mut questions = Vec::new();
    for a in &atoms.atoms {
        match a {
            AtomEnvelope::Claim(c) => claims.push(c.clone()),
            AtomEnvelope::Question(q) => questions.push(q.clone()),
            _ => {}
        }
    }

    let open_questions = count_open_questions(&questions);
    let ungrounded_claims = count_ungrounded_claims(&claims, &edges.edges);
    let transitions_without_trigger = count_transitions_without_trigger(&edges.edges);

    let report = build_schema_validation_report(SchemaValidationInput {
        corpus_id,
        atoms: &atoms,
        edges: &edges,
        cross_corpus: cross_corpus.as_ref(),
        open_questions,
        ungrounded_claims,
        transitions_without_trigger,
    });
    Ok(report)
}

fn atlas_dir_for(corpus_id: &str) -> PathBuf {
    paths::index_root(corpus_id).join(ATLAS_DIRNAME)
}

fn write_atomic<T: serde::Serialize>(path: &Path, value: &T) -> std::io::Result<()> {
    use std::fs;
    let parent = path.parent().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("path has no parent: {}", path.display()),
        )
    })?;
    fs::create_dir_all(parent)?;
    let tmp = parent.join(format!(
        ".{}.tmp",
        path.file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("report")
    ));
    let data =
        serde_json::to_vec_pretty(value).map_err(|e| std::io::Error::other(format!("ser: {e}")))?;
    fs::write(&tmp, data)?;
    fs::rename(&tmp, path)?;
    Ok(())
}

// ── Pretty printer ───────────────────────────────────────────

fn print_human_report(r: &SchemaValidationReport) {
    println!("=== Schema validation — {} ===", r.corpus_id);
    println!();
    println!("  sections with atom evidence: {}", r.section_count);
    println!();

    println!("  [1] Extraction coverage");
    println!("      total atoms: {}", r.extraction.total_atoms);
    for b in &r.extraction.by_type {
        println!("        · {:13} {}", b.atom_type, b.count);
    }
    if !r.extraction.zero_coverage_types.is_empty() {
        println!(
            "      zero-coverage types: {}",
            r.extraction.zero_coverage_types.join(", ")
        );
    }
    println!();

    println!("  [2] Enrichment-depth distribution");
    println!("        · Extracted             {}", r.depth.extracted);
    println!("        · Structural            {}", r.depth.structural);
    println!(
        "        · StructuralClassified  {}",
        r.depth.structural_classified
    );
    println!();

    println!("  [3] Confidence distribution");
    println!(
        "      total atoms with confidence: {}",
        r.confidence.total_with_confidence
    );
    println!(
        "      low-confidence fraction (<0.5): {:.1}%",
        r.confidence.low_confidence_fraction * 100.0
    );
    let max_bucket = r.confidence.buckets.iter().copied().max().unwrap_or(0);
    for (i, &c) in r.confidence.buckets.iter().enumerate() {
        let bar_len = if max_bucket == 0 {
            0
        } else {
            (c as f32 / max_bucket as f32 * 30.0) as usize
        };
        let bar = "█".repeat(bar_len);
        println!(
            "        · [{:.1}–{:.1})  {:>4}  {}",
            i as f32 * 0.1,
            (i + 1) as f32 * 0.1,
            c,
            bar
        );
    }
    println!();

    println!("  [4] Atom-type utilisation");
    for f in &r.utilisation.fractions {
        println!("        · {:13} {:.1}%", f.atom_type, f.fraction * 100.0);
    }
    if !r.utilisation.under_utilised_types.is_empty() {
        println!(
            "      under-utilised types (<3%): {}",
            r.utilisation.under_utilised_types.join(", ")
        );
    }
    println!();

    println!("  [5] Orphan analysis");
    println!(
        "      orphan atoms: {}/{}  ({:.1}% of non-Configuration atoms have no edges)",
        r.orphans.orphan_atoms,
        r.orphans.total_atoms,
        r.orphans.orphan_fraction * 100.0
    );
    for b in &r.orphans.by_type {
        if b.total_count == 0 {
            continue;
        }
        println!(
            "        · {:<12} {:>3}/{:<3}  ({:>4.0}%)",
            b.atom_type,
            b.orphan_count,
            b.total_count,
            b.orphan_fraction * 100.0
        );
    }
    println!();

    println!("  [6] Discourse-act distribution (Claim atoms)");
    println!("      total claims: {}", r.discourse.total_claims);
    if let Some(top) = &r.discourse.top_act {
        println!(
            "      top act: {} ({:.1}%)",
            top,
            r.discourse.top_fraction * 100.0
        );
    }
    for b in &r.discourse.buckets {
        println!("        · {:15} {}", b.act, b.count);
    }
    println!();

    println!("  [7] Cross-corpus connectivity");
    if r.cross_corpus.available {
        println!("      grounding edges: {}", r.cross_corpus.grounding_count);
        println!(
            "      local entity atoms with ≥ 1 outbound edge: {}/{}",
            r.cross_corpus.local_atoms_with_outbound, r.cross_corpus.local_entity_atom_count
        );
    } else {
        println!(
            "      (cross_corpus_edges.json not present — run `sovereign enrich \
             atlas-cross-corpus <this> <peer>` to populate)"
        );
    }
    println!();

    println!("  [8] Deterministic gap counts");
    println!(
        "        · transition_without_trigger: {}/{}",
        r.gaps.transition_without_trigger, r.gaps.total_transitions
    );
    println!(
        "        · ungrounded_claim:           {}/{}",
        r.gaps.ungrounded_claim, r.gaps.total_claims
    );
    println!(
        "        · open_question:              {}/{}",
        r.gaps.open_question, r.gaps.total_questions
    );

    // Collected gap signatures — the single place an operator can
    // see every systematic-issue flag this report detected.
    let sigs = r.gap_signatures();
    if !sigs.is_empty() {
        println!();
        println!("  ⚠ Gap signatures detected ({}):", sigs.len());
        for s in sigs {
            println!("        · {s}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_report_accepts_bare_corpus_id() {
        let p = parse_report_args(&["bk".into()]).unwrap();
        assert_eq!(p.corpus_id, "bk");
        assert!(!p.as_json);
    }

    #[test]
    fn parse_report_accepts_json_flag() {
        let p = parse_report_args(&["bk".into(), "--json".into()]).unwrap();
        assert!(p.as_json);
    }

    #[test]
    fn parse_report_rejects_missing_corpus_id() {
        let err = parse_report_args(&[]).unwrap_err();
        assert!(err.contains("corpus-id"));
    }

    #[test]
    fn parse_report_rejects_extra_positional() {
        let err = parse_report_args(&["bk".into(), "extra".into()]).unwrap_err();
        assert!(err.contains("positional") || err.contains("unexpected"));
    }

    #[test]
    fn parse_report_rejects_unknown_flag() {
        let err = parse_report_args(&["bk".into(), "--nope".into()]).unwrap_err();
        assert!(err.contains("unknown flag"));
    }

    #[test]
    fn parse_review_requires_two_or_more_corpora() {
        let err = parse_review_args(&[]).unwrap_err();
        assert!(err.contains("at least 2"));
        let err = parse_review_args(&["only".into()]).unwrap_err();
        assert!(err.contains("at least 2"));
    }

    #[test]
    fn parse_review_accepts_n_corpora() {
        let p = parse_review_args(&["a".into(), "b".into(), "c".into()]).unwrap();
        assert_eq!(p.corpora, vec!["a", "b", "c"]);
    }

    #[test]
    fn parse_review_rejects_unknown_flag() {
        let err = parse_review_args(&["a".into(), "b".into(), "--nope".into()]).unwrap_err();
        assert!(err.contains("unknown flag"));
    }
}
