// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn enrich errors <corpus> [--phase P] [--kind K] [--json]`
//!
//! One surface for every structured failure across every phase of a
//! corpus's enrichment run. The aggregator walks:
//!
//!   1. `cache/questions.json`          → Phase 1 failures (legacy shape adapted)
//!   2. `cache/atlas-clusters.json`     → Phase 2 atlas failures
//!   3. `cache/atlas-named-clusters.json` → Phase 3 atlas naming failures
//!   4. `cache/concerns.json`           → Phase 3 v1 naming failures
//!   5. `cache/positions.json`          → Phase 5 failures
//!   6. `cache/tensions.json`           → Phase 6 failures
//!   7. `cache/gaps.json`               → Phase 7 failures
//!   8. `atlas/resolution_failures.json` → Phase 3a/3b drops
//!
//! Failures are grouped by `(phase, kind)` with a per-group count,
//! a sample subject, the remediation hint from `PhaseFailureKind`,
//! and a concrete retry command the operator can copy-paste.
//!
//! Why one command and not per-phase `--errors` flags? Operators
//! debugging a new corpus need the failure shape at a glance —
//! "which kind is driving the drops" is the first question, and it
//! crosses phases (e.g. a seed-list regression shows up as
//! UnresolvedEntityName in both Phase 3a and Phase 3b at once).

use std::path::{Path, PathBuf};

use corpus_engine::enrichment::atlas::{ResolutionFailuresFile, ATLAS_DIRNAME};
use corpus_engine::enrichment::pipeline::{
    Phase1Output, Phase2AtlasOutput, Phase2Output, Phase3AtlasOutput, Phase3Output, Phase4Output,
    Phase5Output, Phase6Output, Phase7Output, PhaseFailure, PhaseFailureKind, PipelinePhase,
};

use super::config::EnrichConfig;
use super::paths;
use sovereign_cli_shared::dirs::sovereign_indexes;
use sovereign_cli_shared::help::{self, Help, HelpSection};

const HELP: Help = Help {
    command: "svrn enrich errors",
    summary: "Aggregate structured failures across every phase of a corpus's enrichment run.",
    sections: &[
        HelpSection::Usage(
            "svrn enrich errors <corpus-id> [--phase <phase>] [--kind <kind>] [--json]",
        ),
        HelpSection::Flags(&[
            (
                "--phase <phase>",
                "Filter to failures from one phase (`questions`, `atlas-named-clusters`, \
                 `tensions`, ...). Matches `PipelinePhase::id()`.",
            ),
            (
                "--kind <kind>",
                "Filter to one failure kind (`parse_drift`, `unresolved_entity_name`, ...). \
                 Matches the `PhaseFailureKind` serde name.",
            ),
            (
                "--json",
                "Emit the aggregated failures as pretty JSON instead of a console summary.",
            ),
        ]),
        HelpSection::Examples(&[
            (
                "svrn enrich errors dopesick_jesus",
                "Print every failure group with remediation + retry command.",
            ),
            (
                "svrn enrich errors dopesick_jesus --kind parse_drift",
                "Only parse-drift failures (across every LLM-driven phase).",
            ),
            (
                "svrn enrich errors dopesick_jesus --phase atlas-named-clusters",
                "Only Phase 3 atlas cluster-naming failures.",
            ),
            (
                "svrn enrich errors dopesick_jesus --json",
                "Machine-readable output — pipe into jq, send to the desktop app, etc.",
            ),
        ]),
        HelpSection::Notes(
            "No LLM calls. Reads only cached phase outputs + `atlas/resolution_failures.json`. \
             A corpus that hasn't run Phase 1 yet will report zero failures (and nothing to fix), \
             not an error — run `svrn enrich build <corpus>` first.",
        ),
    ],
};

pub async fn cmd_errors(args: &[String]) -> i32 {
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

    // Verify the corpus is initialised — a missing config file is
    // the most common "first failure" an operator sees. Give a clear
    // hint rather than a stacktrace.
    if let Err(e) = EnrichConfig::require(&parsed.corpus_id) {
        eprintln!("error: {e}");
        return 1;
    }

    let failures = collect_failures(&parsed.corpus_id);

    // Apply filters.
    let filtered: Vec<PhaseFailure> = failures
        .into_iter()
        .filter(|f| match &parsed.phase_filter {
            Some(p) => f.phase == *p,
            None => true,
        })
        .filter(|f| match &parsed.kind_filter {
            Some(k) => f.kind == *k,
            None => true,
        })
        .collect();

    if parsed.json {
        // Enrich each record with the kind's remediation hint so
        // downstream consumers (desktop drawer, scripts) don't
        // re-implement the lookup. The hint is static — derived
        // from `PhaseFailureKind::remediation_hint()` — so there's
        // one source of truth. We don't add `remediation` to the
        // core `PhaseFailure` struct because that would widen the
        // on-disk schema (every cached phase file would rewrite).
        // Flatten-wrap at the CLI boundary instead.
        let views: Vec<PhaseFailureView<'_>> = filtered
            .iter()
            .map(|f| PhaseFailureView {
                inner: f,
                remediation: f.kind.remediation_hint(),
            })
            .collect();
        let json = serde_json::to_string_pretty(&views).unwrap_or_else(|_| "[]".into());
        println!("{json}");
        return 0;
    }

    print_report(&parsed.corpus_id, &filtered);
    0
}

/// JSON-only view that flattens a `PhaseFailure` and appends the
/// `remediation` hint for the UI drawer. Never deserialised —
/// only produced by the `--json` path of the aggregator.
#[derive(serde::Serialize)]
struct PhaseFailureView<'a> {
    #[serde(flatten)]
    inner: &'a PhaseFailure,
    remediation: &'static str,
}

/// Walk every known cache + atlas file for `corpus_id` and pull
/// structured failures into a single flat vector.
///
/// Files that don't exist are skipped silently (a corpus that
/// only ran Phase 1 still reports Phase 1 failures without
/// complaining about missing Phase 5 cache). Files that exist but
/// fail to parse emit a warning to stderr and are skipped — a
/// single corrupt phase file shouldn't take the whole aggregator
/// down.
fn collect_failures(corpus_id: &str) -> Vec<PhaseFailure> {
    let mut out = Vec::new();

    // Phase 1 — cache/questions.json carries Phase1Failure, which
    // the aggregator adapts into the unified shape.
    let cache = paths::cache_dir(corpus_id);
    if let Some(p1) = load_cache::<Phase1Output>(&cache, "questions.json") {
        for f in p1.failures {
            out.push(f.to_phase_failure());
        }
    }

    // v1 Phase 2/3 and chain phases. These caches exist on
    // literary (v1) pipelines; atlas pipelines skip them. Iterating
    // the full list keeps the aggregator pipeline-agnostic.
    if let Some(p2) = load_cache::<Phase2Output>(&cache, "question-clusters.json") {
        out.extend(p2.failures);
    }
    if let Some(p3) = load_cache::<Phase3Output>(&cache, "concerns.json") {
        out.extend(p3.failures);
    }
    if let Some(p4) = load_cache::<Phase4Output>(&cache, "chunk-clusters.json") {
        out.extend(p4.failures);
    }
    if let Some(p5) = load_cache::<Phase5Output>(&cache, "positions.json") {
        out.extend(p5.failures);
    }
    if let Some(p6) = load_cache::<Phase6Output>(&cache, "tensions.json") {
        out.extend(p6.failures);
    }
    if let Some(p7) = load_cache::<Phase7Output>(&cache, "gaps.json") {
        out.extend(p7.failures);
    }

    // Atlas Phase 2 + Phase 3 (atlas cluster naming).
    if let Some(p2a) = load_cache::<Phase2AtlasOutput>(&cache, "atlas-clusters.json") {
        out.extend(p2a.failures);
    }
    if let Some(p3a) = load_cache::<Phase3AtlasOutput>(&cache, "atlas-named-clusters.json") {
        out.extend(p3a.failures);
    }

    // Phase 3a/3b resolution drops live in the atlas directory,
    // not the enrichment cache (they're written by `enrich resolve`
    // alongside atoms.json / edges.json).
    let atlas_dir = sovereign_indexes().join(corpus_id).join(ATLAS_DIRNAME);
    match ResolutionFailuresFile::load(&atlas_dir) {
        Ok(Some(f)) => out.extend(f.failures),
        Ok(None) => {} // atlas hasn't been written yet; that's fine
        Err(e) => {
            eprintln!(
                "warning: reading resolution_failures.json under {}: {e}",
                atlas_dir.display()
            );
        }
    }

    out
}

fn load_cache<T>(cache_dir: &Path, filename: &str) -> Option<T>
where
    T: serde::de::DeserializeOwned,
{
    let path = cache_dir.join(filename);
    if !path.exists() {
        return None;
    }
    match std::fs::read_to_string(&path) {
        Ok(raw) => match serde_json::from_str::<T>(&raw) {
            Ok(parsed) => Some(parsed),
            Err(e) => {
                eprintln!(
                    "warning: parsing {}: {e} (skipping — rerun the phase to refresh)",
                    path.display()
                );
                None
            }
        },
        Err(e) => {
            eprintln!("warning: reading {}: {e} (skipping)", path.display());
            None
        }
    }
}

/// Group failures by (phase, kind) and print a glassbox report:
///
/// ```text
/// === Enrichment errors — <corpus> ===
///
/// 152 total failure(s) across 4 kind(s)
///
///   [questions / unresolved_entity_name] — 73 failure(s)
///     Sample: `sketch:entity_state:sec_0017#3` — entity-state sketch references unknown entity `Gabe Sharma`...
///     Remediate: Fuzzy resolver couldn't match...
///     Retry: svrn enrich atlas-resolve <corpus> --phase all
///
///   [atlas-named-clusters / parse_drift] — 4 failure(s)
///     Sample: `cluster:claim:cl_c_14` — parse error naming cluster cl_c_14...
///     Remediate: Retry with `svrn enrich extract <corpus> --retry-failed`...
///     Retry: svrn enrich name-atlas-clusters <corpus>
/// ```
///
/// Groups are sorted by count descending so the biggest signal
/// lands at the top.
fn print_report(corpus_id: &str, failures: &[PhaseFailure]) {
    println!();
    println!("=== Enrichment errors — {corpus_id} ===");
    println!();

    if failures.is_empty() {
        println!("  ✓ no structured failures across any phase");
        println!();
        println!(
            "  If the run didn't feel clean, check stderr output from `enrich build` — \
             deterministic phases log non-failure warnings there."
        );
        return;
    }

    let groups = group_by_phase_kind(failures);
    println!(
        "  {} total failure(s) across {} group(s)",
        failures.len(),
        groups.len()
    );
    println!();

    for group in groups {
        let (phase, kind, items) = group;
        println!(
            "  [{} / {}] — {} failure(s)",
            phase.id(),
            kind_as_str(kind),
            items.len()
        );
        // Sample: the first item's subject + one-line reason.
        if let Some(sample) = items.first() {
            let reason = one_line(&sample.reason, 100);
            println!("    Sample: `{}` — {}", sample.subject, reason);
        }
        println!("    Remediate: {}", kind.remediation_hint());
        if let Some(retry) = retry_command(phase, corpus_id) {
            println!("    Retry:     {retry}");
        }
        println!();
    }
}

/// Stable grouping: sort by count descending, then by phase id,
/// then by kind for ties. The sort order is documented because the
/// aggregator is the primary surface an operator reads, and
/// reshuffling a report between runs would make regressions hard
/// to spot.
fn group_by_phase_kind(
    failures: &[PhaseFailure],
) -> Vec<(PipelinePhase, PhaseFailureKind, Vec<&PhaseFailure>)> {
    use std::collections::HashMap;
    // HashMap keyed on (phase_id, kind_str) because PipelinePhase and
    // PhaseFailureKind are Eq + Hash but not Ord — we sort the groups
    // explicitly below rather than rely on a BTreeMap's ordering, so
    // the sort contract is visible in one place.
    let mut map: HashMap<(PipelinePhase, PhaseFailureKind), Vec<&PhaseFailure>> = HashMap::new();
    for f in failures {
        map.entry((f.phase, f.kind)).or_default().push(f);
    }
    let mut out: Vec<_> = map.into_iter().map(|((p, k), v)| (p, k, v)).collect();
    out.sort_by(|a, b| {
        b.2.len()
            .cmp(&a.2.len())
            .then_with(|| a.0.id().cmp(b.0.id()))
            .then_with(|| kind_as_str(a.1).cmp(kind_as_str(b.1)))
    });
    out
}

/// Best-effort retry command per phase. Not every phase has a
/// standalone CLI entry point; when there isn't one, return None
/// and the printer omits the line.
fn retry_command(phase: PipelinePhase, corpus_id: &str) -> Option<String> {
    match phase {
        PipelinePhase::Questions => Some(format!("svrn enrich extract {corpus_id} --retry-failed")),
        PipelinePhase::Concerns => Some(format!("svrn enrich name-concerns {corpus_id}")),
        PipelinePhase::AtlasClusters => Some(format!("svrn enrich cluster-atlas {corpus_id}")),
        PipelinePhase::AtlasNamedClusters => {
            Some(format!("svrn enrich name-atlas-clusters {corpus_id}"))
        }
        PipelinePhase::Positions => Some(format!("svrn enrich extract-positions {corpus_id}")),
        PipelinePhase::Tensions => Some(format!("svrn enrich detect-tensions {corpus_id}")),
        PipelinePhase::Gaps => Some(format!("svrn enrich detect-gaps {corpus_id}")),
        PipelinePhase::SeedExtraction => Some(format!("svrn enrich seed {corpus_id} --force")),
        PipelinePhase::Ingest | PipelinePhase::QuestionClusters | PipelinePhase::ChunkClusters => {
            None
        }
    }
}

fn kind_as_str(kind: PhaseFailureKind) -> &'static str {
    match kind {
        PhaseFailureKind::ThinkTruncated => "think_truncated",
        PhaseFailureKind::ParseDrift => "parse_drift",
        PhaseFailureKind::ChatError => "chat_error",
        PhaseFailureKind::DeadlineExceeded => "deadline_exceeded",
        PhaseFailureKind::EmptyExtraction => "empty_extraction",
        PhaseFailureKind::Skipped => "skipped",
        PhaseFailureKind::UnresolvedEntityName => "unresolved_entity_name",
        PhaseFailureKind::EntityMergeAmbiguous => "entity_merge_ambiguous",
        PhaseFailureKind::UnresolvedRelationParticipant => "unresolved_relation_participant",
        PhaseFailureKind::UnresolvedClaimAttribution => "unresolved_claim_attribution",
        PhaseFailureKind::NoClusterableItems => "no_clusterable_items",
        PhaseFailureKind::ClusterNamingFailed => "cluster_naming_failed",
        PhaseFailureKind::Other => "other",
    }
}

fn parse_kind(s: &str) -> Option<PhaseFailureKind> {
    // Match the same snake_case shape that serde produces.
    match s {
        "think_truncated" => Some(PhaseFailureKind::ThinkTruncated),
        "parse_drift" => Some(PhaseFailureKind::ParseDrift),
        "chat_error" => Some(PhaseFailureKind::ChatError),
        "deadline_exceeded" => Some(PhaseFailureKind::DeadlineExceeded),
        "empty_extraction" => Some(PhaseFailureKind::EmptyExtraction),
        "skipped" => Some(PhaseFailureKind::Skipped),
        "unresolved_entity_name" => Some(PhaseFailureKind::UnresolvedEntityName),
        "entity_merge_ambiguous" => Some(PhaseFailureKind::EntityMergeAmbiguous),
        "unresolved_relation_participant" => Some(PhaseFailureKind::UnresolvedRelationParticipant),
        "unresolved_claim_attribution" => Some(PhaseFailureKind::UnresolvedClaimAttribution),
        "no_clusterable_items" => Some(PhaseFailureKind::NoClusterableItems),
        "cluster_naming_failed" => Some(PhaseFailureKind::ClusterNamingFailed),
        "other" => Some(PhaseFailureKind::Other),
        _ => None,
    }
}

fn one_line(s: &str, cap: usize) -> String {
    let flat: String = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= cap {
        flat
    } else {
        flat.chars().take(cap.saturating_sub(1)).collect::<String>() + "…"
    }
}

#[derive(Debug)]
struct ParsedErrors {
    corpus_id: String,
    phase_filter: Option<PipelinePhase>,
    kind_filter: Option<PhaseFailureKind>,
    json: bool,
}

fn parse_args(args: &[String]) -> Result<ParsedErrors, String> {
    use std::str::FromStr;

    let mut corpus_id: Option<String> = None;
    let mut phase_filter: Option<PipelinePhase> = None;
    let mut kind_filter: Option<PhaseFailureKind> = None;
    let mut json = false;

    let mut i = 0;
    while i < args.len() {
        let a = &args[i];
        match a.as_str() {
            "--phase" => {
                let v = args
                    .get(i + 1)
                    .ok_or_else(|| "--phase requires a phase id".to_string())?;
                phase_filter =
                    Some(PipelinePhase::from_str(v).map_err(|e| format!("--phase: {e}"))?);
                i += 2;
            }
            "--kind" => {
                let v = args
                    .get(i + 1)
                    .ok_or_else(|| "--kind requires a kind name".to_string())?;
                kind_filter =
                    Some(parse_kind(v).ok_or_else(|| format!("--kind: unknown kind `{v}`"))?);
                i += 2;
            }
            "--json" => {
                json = true;
                i += 1;
            }
            other if other.starts_with("--") => {
                return Err(format!("unknown flag: {other}"));
            }
            other => {
                if corpus_id.is_none() {
                    corpus_id = Some(other.to_string());
                    i += 1;
                } else {
                    return Err(format!("unexpected positional argument: {other}"));
                }
            }
        }
    }
    let corpus_id = corpus_id.ok_or_else(|| "missing <corpus-id>".to_string())?;
    Ok(ParsedErrors {
        corpus_id,
        phase_filter,
        kind_filter,
        json,
    })
}

// Silence the unused-import warning for PathBuf — reserved for a
// future `--json <path>` flag that writes the report to disk for
// the desktop app to pick up.
#[allow(dead_code)]
fn _reserved() -> Option<PathBuf> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn failure(
        phase: PipelinePhase,
        kind: PhaseFailureKind,
        subject: &str,
        reason: &str,
    ) -> PhaseFailure {
        PhaseFailure {
            phase,
            subject: subject.into(),
            kind,
            reason: reason.into(),
            raw_response_head: None,
        }
    }

    #[test]
    fn group_by_phase_kind_sorts_groups_by_count_descending() {
        let failures = vec![
            failure(
                PipelinePhase::Questions,
                PhaseFailureKind::ParseDrift,
                "chapter:sec_0001",
                "x",
            ),
            failure(
                PipelinePhase::Questions,
                PhaseFailureKind::ParseDrift,
                "chapter:sec_0002",
                "x",
            ),
            failure(
                PipelinePhase::Questions,
                PhaseFailureKind::ParseDrift,
                "chapter:sec_0003",
                "x",
            ),
            failure(
                PipelinePhase::AtlasNamedClusters,
                PhaseFailureKind::ChatError,
                "cluster:claim:cl_1",
                "x",
            ),
        ];
        let groups = group_by_phase_kind(&failures);
        assert_eq!(groups.len(), 2);
        // Largest first: ParseDrift (3) before ChatError (1).
        assert_eq!(groups[0].0, PipelinePhase::Questions);
        assert_eq!(groups[0].1, PhaseFailureKind::ParseDrift);
        assert_eq!(groups[0].2.len(), 3);
        assert_eq!(groups[1].0, PipelinePhase::AtlasNamedClusters);
    }

    #[test]
    fn retry_command_gives_concrete_command_for_llm_phases() {
        // Every LLM-driven phase has an entry point the operator
        // can re-run. Deterministic phases (Ingest, QuestionClusters,
        // ChunkClusters) have no standalone retry surface — the
        // aggregator correctly returns None so it doesn't print a
        // misleading command.
        assert!(retry_command(PipelinePhase::Questions, "bk").is_some());
        assert!(retry_command(PipelinePhase::AtlasNamedClusters, "bk").is_some());
        assert!(retry_command(PipelinePhase::Tensions, "bk").is_some());
        assert!(retry_command(PipelinePhase::Ingest, "bk").is_none());
        assert!(retry_command(PipelinePhase::QuestionClusters, "bk").is_none());
    }

    #[test]
    fn parse_kind_matches_serde_snake_case() {
        // The CLI filter `--kind parse_drift` has to match the exact
        // serialisation the JSON-on-disk uses. If PhaseFailureKind's
        // serde rename_all attr changes, this test breaks loudly.
        assert_eq!(
            parse_kind("parse_drift"),
            Some(PhaseFailureKind::ParseDrift)
        );
        assert_eq!(
            parse_kind("unresolved_entity_name"),
            Some(PhaseFailureKind::UnresolvedEntityName)
        );
        assert_eq!(parse_kind("nonsense_kind"), None);
    }

    #[test]
    fn parse_args_requires_corpus_id() {
        let err = parse_args(&[]).unwrap_err();
        assert!(err.contains("corpus"));
    }

    #[test]
    fn parse_args_accepts_filters() {
        let p = parse_args(&[
            "bk".into(),
            "--phase".into(),
            "questions".into(),
            "--kind".into(),
            "parse_drift".into(),
            "--json".into(),
        ])
        .unwrap();
        assert_eq!(p.corpus_id, "bk");
        assert_eq!(p.phase_filter, Some(PipelinePhase::Questions));
        assert_eq!(p.kind_filter, Some(PhaseFailureKind::ParseDrift));
        assert!(p.json);
    }

    #[test]
    fn parse_args_rejects_unknown_kind() {
        let err = parse_args(&["bk".into(), "--kind".into(), "nonsense_kind".into()]).unwrap_err();
        assert!(err.contains("unknown kind"));
    }

    /// Glassbox integration test: scoped HOME with a dirty
    /// `cache/questions.json` + `atlas/resolution_failures.json`
    /// proves that `collect_failures` pulls from both sources and
    /// unifies them into one `Vec<PhaseFailure>` — a regression in
    /// either the legacy Phase1 adapter or the atlas failure reader
    /// fails this test loudly.
    #[test]
    fn collect_failures_unifies_cache_and_atlas_failure_sources() {
        // Use the shared `test_env::scoped_home` guard (defined in
        // `enrich_cmd/mod.rs`) rather than a module-local mutex so
        // this test serialises with every *other* HOME-scoping test
        // in the crate. Two independent mutexes would let tests
        // collide on the process-wide `HOME` env var, causing
        // flakiness like the one observed on the first full-suite
        // run after Landing 3.C added a second mutex here.
        use crate::enrich_cmd::test_env::scoped_home;
        use corpus_engine::enrichment::atlas::{write_atlas_failures, ATLAS_DIRNAME};
        use corpus_engine::enrichment::pipeline::{Phase1Failure, Phase1Output, PhaseCache};
        use std::fs;

        // RAII: keep the scoped HOME override alive for the whole test (its
        // Drop restores HOME). We no longer read `guard.path()` directly —
        // paths resolve through `sovereign_indexes()` under the scoped HOME.
        let _guard = scoped_home();

        // Minimal config so `EnrichConfig::require` wouldn't trip —
        // not used by collect_failures directly but the caller
        // checks it upstream.
        let corpus_id = "smoke_dirty";
        let cache_dir = paths::cache_dir(corpus_id);
        fs::create_dir_all(&cache_dir).unwrap();

        // Seed a Phase 1 cache with two failures.
        let p1 = Phase1Output {
            schema_version: Phase1Output::SCHEMA_VERSION,
            pipeline_id: "literary_atlas".into(),
            questions_by_chapter: Vec::new(),
            failures: vec![
                Phase1Failure {
                    chapter_id: "sec_0001".into(),
                    reason: "<think> truncated".into(),
                    raw_response_head: None,
                    failure_kind: PhaseFailureKind::ThinkTruncated,
                },
                Phase1Failure {
                    chapter_id: "sec_0005".into(),
                    reason: "parse drift: EOF mid-json".into(),
                    raw_response_head: Some("{\"entities\":[{...".into()),
                    failure_kind: PhaseFailureKind::ParseDrift,
                },
            ],
            written_at: "t".into(),
        };
        let cache = PhaseCache::new(&cache_dir);
        cache.write(PipelinePhase::Questions, &p1).unwrap();

        // Seed an atlas/resolution_failures.json with two more.
        // Resolve via the same getter the code under test uses, rather than
        // hard-coding the legacy `.sovereign` dir name — `sovereign_indexes()`
        // prefers `~/.svrnmesh` once it's populated (the cache write above does
        // that), so a hard-coded `.sovereign` path would miss the atlas source.
        let indexes_root = sovereign_cli_shared::dirs::sovereign_indexes();
        let atlas_dir = indexes_root.join(corpus_id).join(ATLAS_DIRNAME);
        let atlas_failures = vec![
            PhaseFailure {
                phase: PipelinePhase::Questions,
                subject: "sketch:entity_state:sec_0003#2".into(),
                kind: PhaseFailureKind::UnresolvedEntityName,
                reason: "entity `Gabe Sharma` did not resolve".into(),
                raw_response_head: None,
            },
            PhaseFailure {
                phase: PipelinePhase::Questions,
                subject: "sketch:claim:sec_0010#1".into(),
                kind: PhaseFailureKind::UnresolvedClaimAttribution,
                reason: "attributed_to `The Publisher` did not resolve".into(),
                raw_response_head: None,
            },
        ];
        write_atlas_failures(&atlas_dir, &atlas_failures).unwrap();

        let collected = collect_failures(corpus_id);

        // 4 total: 2 from cache, 2 from atlas.
        assert_eq!(
            collected.len(),
            4,
            "expected 4 failures (2 cache + 2 atlas), got {}",
            collected.len()
        );
        // Every kind shows up.
        let kinds: Vec<PhaseFailureKind> = collected.iter().map(|f| f.kind).collect();
        assert!(kinds.contains(&PhaseFailureKind::ThinkTruncated));
        assert!(kinds.contains(&PhaseFailureKind::ParseDrift));
        assert!(kinds.contains(&PhaseFailureKind::UnresolvedEntityName));
        assert!(kinds.contains(&PhaseFailureKind::UnresolvedClaimAttribution));
        // Phase-1 legacy subjects gain the `chapter:` prefix via
        // `Phase1Failure::to_phase_failure`.
        assert!(collected.iter().any(|f| f.subject == "chapter:sec_0001"));
    }
}

// Earlier iterations defined a module-local `mod_integration_helpers`
// with its own HOME_LOCK mutex. That duplicated the lock defined in
// `enrich_cmd::test_env` (mod.rs) and let tests in the two modules
// collide on the process-wide `HOME` env var when run
// concurrently. The helper was consolidated into `test_env` —
// nothing lives here now by design.
