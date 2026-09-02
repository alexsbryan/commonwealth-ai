// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn enrich schema-report` and `schema-review` —
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
    read_atlas_cross_corpus_edges, read_atlas_edges, read_atlas_ontology, AtomEnvelope,
    SchemaComparison, SchemaValidationInput, SchemaValidationReport, ATLAS_DIRNAME,
};

use super::config::EnrichConfig;
use super::paths;

// ── schema-report ────────────────────────────────────────────

/// A parsed `schema-report` invocation. Public so the `enrich build`
/// orchestrator constructs one directly instead of round-tripping
/// through argv.
#[derive(Debug, Clone)]
pub struct ParsedReport {
    pub corpus_id: String,
    pub as_json: bool,
}

/// Where the JSON artifact landed — or why it did not.
///
/// The write is non-fatal (the table still prints), but "wrote it" and
/// "could not write it" are different outcomes. Until 2026-08-26 the
/// failure went to stderr as a `warning:` and the step still reported
/// plain success, so no caller could tell the two apart (ARCH §18.3).
#[derive(Debug, Clone)]
pub enum ReportArtifact {
    Wrote(PathBuf),
    Failed { path: PathBuf, error: String },
}

/// What `schema-report` produced.
#[derive(Debug, Clone)]
pub struct ReportRun {
    pub report: SchemaValidationReport,
    pub artifact: ReportArtifact,
}

/// Format the step's one-line summary from what the run found.
///
/// A free function rather than a method so it is exercisable on its own:
/// `SchemaValidationReport`'s nine sub-structs have no `Default` (by
/// design — an all-zero report is not a real outcome), so a test that
/// had to build one to check a sentence would be pressure to add one.
fn summary_line(
    total_atoms: usize,
    section_count: usize,
    gap_signatures: usize,
    artifact: &ReportArtifact,
) -> String {
    let base = format!(
        "{total_atoms} atom(s) over {section_count} section(s); \
         {gap_signatures} schema gap signature(s)"
    );
    match artifact {
        ReportArtifact::Wrote(_) => base,
        ReportArtifact::Failed { path, error } => {
            format!("{base} — could not write {}: {error}", path.display())
        }
    }
}

impl ReportRun {
    /// One line naming what this step found, for the build
    /// orchestrator's `StepDone` event.
    pub fn summary(&self) -> String {
        summary_line(
            self.report.extraction.total_atoms,
            self.report.section_count,
            self.report.gap_signatures().len(),
            &self.artifact,
        )
    }
}

/// Compute the §12 report and write the JSON artifact. Pure of stdout:
/// the table comes from [`render`].
pub fn run(parsed: &ParsedReport) -> Result<ReportRun, String> {
    let cfg = EnrichConfig::require(&parsed.corpus_id)
        .map_err(|e| format!("loading enrichment config: {e}"))?;
    let report = compute_report(&cfg.corpus_id)?;

    // Written alongside the other atlas artifacts so downstream
    // consumers (CI, dashboards) can pick it up.
    let out_path = atlas_dir_for(&cfg.corpus_id).join("schema_validation.json");
    let artifact = match write_atomic(&out_path, &report) {
        Ok(()) => ReportArtifact::Wrote(out_path),
        Err(e) => ReportArtifact::Failed {
            path: out_path,
            error: e.to_string(),
        },
    };

    Ok(ReportRun { report, artifact })
}

/// Print the report the way `svrn enrich schema-report` always has.
pub fn render(parsed: &ParsedReport, run: &ReportRun) -> Result<(), String> {
    if let ReportArtifact::Failed { path, error } = &run.artifact {
        eprintln!("warning: writing {}: {error}", path.display());
    }
    if parsed.as_json {
        let s = serde_json::to_string_pretty(&run.report)
            .map_err(|e| format!("serialising report: {e}"))?;
        println!("{s}");
    } else {
        print_human_report(&run.report);
        if let ReportArtifact::Wrote(path) = &run.artifact {
            println!();
            println!("  ✓ wrote {}", path.display());
        }
    }
    Ok(())
}

pub fn parse_report_args(args: &[String]) -> Result<ParsedReport, String> {
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

#[derive(Debug)]
/// A parsed `enrich review` invocation. `pub` for the same reason as
/// [`ParsedExtract`], with the same private fields: the corpus list is an
/// input to `run_review`, not a surface for a caller to walk.
pub struct ParsedReview {
    corpora: Vec<String>,
}

/// The `review` verb's work: compute one report per corpus, then compare them.
///
/// The other half of this module's verb triple, added when the crate split
/// made the shape matter. `report` always had `run` + `render`; `review` did
/// not — its loop lived inside `cmd_schema_review`, which is why the split
/// initially dragged it up into `sovereign-cli-llm` along with the help text.
/// Comparing schemas across corpora is work, not user interface, so it lives
/// here and `ParsedReview`'s fields stay private.
pub fn run_review(parsed: &ParsedReview) -> Result<SchemaComparison, String> {
    let mut reports = Vec::new();
    for corpus_id in &parsed.corpora {
        let cfg = EnrichConfig::require(corpus_id)
            .map_err(|e| format!("loading config for `{corpus_id}`: {e}"))?;
        reports.push(compute_report(&cfg.corpus_id)?);
    }
    Ok(compare_across_corpora(&reports))
}

/// Print a [`run_review`] comparison in the shape operators have seen since
/// the verb shipped.
pub fn render_review(comparison: &SchemaComparison) {
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
}

pub fn parse_review_args(args: &[String]) -> Result<ParsedReview, String> {
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
pub fn compute_report(corpus_id: &str) -> Result<SchemaValidationReport, String> {
    let atlas_dir = atlas_dir_for(corpus_id);
    let atoms = read_atlas_atoms(&atlas_dir).map_err(|e| {
        format!(
            "reading {}/atoms.json: {e}. Run `svrn enrich atlas-resolve \
             {corpus_id} --phase all` first.",
            atlas_dir.display()
        )
    })?;
    let edges = read_atlas_edges(&atlas_dir).map_err(|err| {
        format!(
            "reading {}/edges.json: {err}. Run `svrn enrich atlas-resolve \
             {corpus_id} --phase all` first.",
            atlas_dir.display()
        )
    })?;
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

    // The declaration this atlas was built under, and what the reconciler did
    // with it. Both optional and both read from the atlas dir: a corpus that
    // declares nothing gets the eight dimensions it always got, and a declared
    // corpus that has never been reconciled reports the merge count as absent
    // rather than as zero.
    let ontology = read_atlas_ontology(&atlas_dir);
    let merges = read_merge_count(&atlas_dir);

    let report = build_schema_validation_report(SchemaValidationInput {
        corpus_id,
        atoms: &atoms,
        edges: &edges,
        cross_corpus: cross_corpus.as_ref(),
        open_questions,
        ungrounded_claims,
        transitions_without_trigger,
        ontology: ontology.as_ref().map(|o| (o, merges)),
    });
    Ok(report)
}

/// `merged_entity_count` from `atlas/reconciliation.json`, or `None` when the
/// file is absent or unreadable. Absence is reported as absence — a corpus
/// that has not been reconciled has not merged zero things, it has not been
/// asked (§18.3).
fn read_merge_count(atlas_dir: &Path) -> Option<usize> {
    let raw = std::fs::read(atlas_dir.join("reconciliation.json")).ok()?;
    let parsed: serde_json::Value = serde_json::from_slice(&raw).ok()?;
    parsed
        .get("merged_entity_count")
        .and_then(serde_json::Value::as_u64)
        .map(|n| n as usize)
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
            "      (cross_corpus_edges.json not present — run `svrn enrich \
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

    if let Some(o) = &r.ontology {
        println!();
        println!("  [9] Declared ontology (version {})", o.ontology_version);
        println!("      per-type coverage");
        for t in &o.by_type {
            // Two numbers, because they answer different questions: how many
            // atoms ARE a `coin`, and how many count AS one once `sceatta`
            // is included. A zero in the second is the headline failure.
            println!(
                "        · {:10} {:14} {:>6}  (with subtypes {})",
                t.kind, t.name, t.count, t.count_with_subtypes
            );
        }
        println!("      identity criteria");
        for i in &o.identity {
            println!("        · {:14} {}", i.type_name, i.criterion);
        }
        match o.merges {
            Some(n) => println!("      merges: {n}"),
            None => println!(
                "      merges: not run (`svrn enrich reconcile {}`)",
                r.corpus_id
            ),
        }
        println!("      same_as claims: {}", o.same_as_claims);
        println!(
            "      claims of a subject-declaring type with no subject: {}",
            o.claims_missing_subject
        );
        if !o.attribute_fill.is_empty() {
            // The type count above says the noun landed; this says whether it
            // landed carrying anything. A row reading `0/14` is a type that
            // exists in name only, and it is invisible in every other line of
            // this report.
            println!("      declared attributes filled");
            for f in &o.attribute_fill {
                let note = if f.atoms > 0 && f.with_slot == 0 {
                    "  ← no attribute slot on this atom kind (role_of lands as a State)"
                } else if f.with_slot > 0 && f.filled == 0 {
                    "  ← declared and never filled"
                } else {
                    ""
                };
                println!(
                    "        · {:10} {:16} {:>4}/{}{}",
                    f.type_name, f.attribute, f.filled, f.with_slot, note
                );
            }
        }
    }

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
mod artifact_tests {
    use super::*;

    /// A failed artifact write used to print `warning:` to stderr and let
    /// the step report plain success — so no caller (the desktop's
    /// progress panel included) could tell "wrote schema_validation.json"
    /// from "could not write it" (ARCH §18.3).
    ///
    /// Falsifier: collapse `ReportArtifact` back to a bare path, or drop
    /// the `Failed` arm from `summary_line`, and the two lines become
    /// equal.
    #[test]
    fn a_failed_artifact_write_is_visible_in_the_summary() {
        let path = PathBuf::from("/x/schema_validation.json");
        let wrote = summary_line(120, 9, 2, &ReportArtifact::Wrote(path.clone()));
        let failed = summary_line(
            120,
            9,
            2,
            &ReportArtifact::Failed {
                path,
                error: "permission denied".into(),
            },
        );
        assert_ne!(wrote, failed);
        assert!(failed.contains("permission denied"), "{failed}");
    }

    /// The summary must vary with what the report FOUND, not with the
    /// step's name. Two runs over different-sized corpora are different
    /// outcomes and must read differently.
    #[test]
    fn the_summary_varies_with_what_the_report_found() {
        let ok = ReportArtifact::Wrote(PathBuf::from("/x.json"));
        let small = summary_line(3, 1, 0, &ok);
        let large = summary_line(4021, 312, 17, &ok);
        assert_ne!(small, large);
        assert!(large.contains("4021"), "{large}");
        assert!(large.contains("312"), "{large}");
        assert!(large.contains("17"), "{large}");
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
