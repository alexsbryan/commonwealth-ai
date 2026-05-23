//! `sovereign enrich show <corpus> <target>` — formatted view of a cached
//! phase output. Landing 2 implements `phase1` (with optional
//! `--chapter <id>` filter); other targets land incrementally.

use corpus_engine::enrichment::pipeline::{
    Phase1Output, Phase2Output, Phase3Output, Phase4Output, Phase5Output, Phase6Output,
    Phase7Output, PhaseCache, PipelinePhase,
};

use super::config::EnrichConfig;
use super::paths;
use crate::util::help::{self, Help, HelpSection};

const HELP: Help = Help {
    command: "sovereign enrich show",
    summary: "Inspect cached phase output without opening JSON files.",
    sections: &[
        HelpSection::Usage(
            "sovereign enrich show <corpus-id> <target> [--chapter <id>]",
        ),
        HelpSection::Subcommands(&[
            ("phase1", "Per-chapter questions from the last --full run."),
            ("question-clusters", "Phase 2 cluster assignments (+ unclustered)."),
            ("concerns", "Phase 3 canonical concerns."),
            ("chunk-clusters", "Phase 4 chunk cluster ids and sizes."),
            ("positions", "Phase 5 grounded positions (use --concern to filter)."),
            ("tensions", "Phase 6 detected tensions."),
            ("gaps", "Phase 7 identified gaps."),
        ]),
        HelpSection::Flags(&[
            ("--chapter <id>", "Filter phase1 output to one chapter id."),
            ("--concern <id>", "Filter positions or tensions by concern id."),
        ]),
    ],
};

pub async fn cmd_show(args: &[String]) -> i32 {
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

    // Ensure config exists (so we fail early with a useful message).
    if let Err(e) = EnrichConfig::require(&parsed.corpus_id) {
        eprintln!("error: {e}");
        return 1;
    }

    let cache = PhaseCache::new(paths::cache_dir(&parsed.corpus_id));
    let code = match parsed.target {
        Target::Phase1 => show_one(&cache, PipelinePhase::Questions, |out: Phase1Output| {
            print_phase1(&out, parsed.chapter.as_deref())
        }),
        Target::Phase2 => {
            show_one(&cache, PipelinePhase::QuestionClusters, |out: Phase2Output| {
                print_phase2(&out)
            })
        }
        Target::Phase3 => show_one(&cache, PipelinePhase::Concerns, |out: Phase3Output| {
            print_phase3(&out)
        }),
        Target::Phase4 => show_one(&cache, PipelinePhase::ChunkClusters, |out: Phase4Output| {
            print_phase4(&out)
        }),
        Target::Phase5 => show_one(&cache, PipelinePhase::Positions, |out: Phase5Output| {
            print_phase5(&out, parsed.concern.as_deref())
        }),
        Target::Phase6 => show_one(&cache, PipelinePhase::Tensions, |out: Phase6Output| {
            print_phase6(&out, parsed.concern.as_deref())
        }),
        Target::Phase7 => show_one(&cache, PipelinePhase::Gaps, |out: Phase7Output| {
            print_phase7(&out)
        }),
    };
    code
}

fn show_one<T, F>(cache: &PhaseCache, phase: PipelinePhase, render: F) -> i32
where
    T: serde::de::DeserializeOwned,
    F: FnOnce(T),
{
    match cache.read::<T>(phase) {
        Ok(Some(out)) => {
            render(out);
            0
        }
        Ok(None) => {
            eprintln!(
                "error: no cached output for phase '{}' — run the corresponding subcommand first",
                phase.id()
            );
            1
        }
        Err(e) => {
            eprintln!("error: reading cache for phase '{}': {e}", phase.id());
            1
        }
    }
}

fn print_phase1(out: &Phase1Output, filter: Option<&str>) {
    println!("Phase 1 (per-chapter questions) — cached {}", out.written_at);
    println!("Pipeline: {} · schema v{}", out.pipeline_id, out.schema_version);
    println!();
    let mut shown = 0usize;
    for entry in &out.questions_by_chapter {
        if let Some(f) = filter {
            if entry.chapter_id != f {
                continue;
            }
        }
        shown += 1;
        println!("  {}", entry.chapter_id);
        for q in &entry.questions {
            println!("    · {q}");
        }
        if let Some(r) = &entry.reveals {
            println!("    reveals: {r}");
        }
        if !entry.thematic_carriers.is_empty() {
            println!("    carriers: {}", entry.thematic_carriers.join(", "));
        }
        if let Some(s) = &entry.setting {
            println!("    setting: {s}");
        }
        if let Some(p) = &entry.plot {
            println!("    plot: {p}");
        }
        println!();
    }
    if shown == 0 {
        if let Some(f) = filter {
            eprintln!("(no cached questions for chapter '{f}')");
        } else {
            eprintln!("(cache is empty)");
        }
    } else {
        println!("  total: {shown} chapter(s)");
    }
    if !out.failures.is_empty() && filter.is_none() {
        println!();
        println!("  failures: {}", out.failures.len());
        for f in &out.failures {
            println!("    · {} — {}", f.chapter_id, f.reason);
        }
    }
}

fn print_phase2(out: &Phase2Output) {
    println!("Phase 2 (question clusters) — cached {}", out.written_at);
    println!(
        "{} cluster(s), {} unclustered",
        out.clusters.len(),
        out.unclustered.len()
    );
    for c in &out.clusters {
        println!("  {} ({} members)", c.id, c.question_refs.len());
    }
    if !out.unclustered.is_empty() {
        println!("\n  unclustered ({}):", out.unclustered.len());
        for r in out.unclustered.iter().take(10) {
            println!("    · {}[{}]", r.chapter_id, r.question_index);
        }
    }
}

fn print_phase3(out: &Phase3Output) {
    println!("Phase 3 (canonical concerns) — cached {}", out.written_at);
    for c in &out.concerns {
        println!("  {} (from {})", c.id, c.cluster_id);
        println!("    {}", c.concern_text);
        if let Some(s) = &c.scope {
            println!("    scope: {s}");
        }
        if !c.primary_arcs.is_empty() {
            println!("    arcs: {}", c.primary_arcs.join(", "));
        }
        println!();
    }
}

fn print_phase4(out: &Phase4Output) {
    println!("Phase 4 (chunk clusters) — cached {}", out.written_at);
    let mut clustered = 0usize;
    let mut noise = 0usize;
    for c in &out.clusters {
        if c.noise {
            noise = c.chunk_ids.len();
        } else {
            clustered += c.chunk_ids.len();
        }
    }
    println!(
        "{} cluster(s), {clustered} clustered chunks, {noise} noise chunks",
        out.clusters.iter().filter(|c| !c.noise).count()
    );
    for c in &out.clusters {
        if c.noise {
            println!("  {} (noise, {} chunks)", c.id, c.chunk_ids.len());
        } else {
            println!("  {} ({} chunks)", c.id, c.chunk_ids.len());
        }
    }
}

fn print_phase5(out: &Phase5Output, concern_filter: Option<&str>) {
    println!("Phase 5 (positions) — cached {}", out.written_at);
    let mut shown = 0usize;
    for p in &out.positions {
        if let Some(cid) = concern_filter {
            if p.concern_id != cid {
                continue;
            }
        }
        shown += 1;
        println!("  {} (concern {}, cluster {})", p.id, p.concern_id, p.chunk_cluster_id);
        println!("    {}", p.position_text);
        if !p.grounding.is_empty() {
            println!("    grounding:");
            for g in &p.grounding {
                println!(
                    "      · chunk {} ({}) — {}",
                    g.chunk_id, g.section_id, g.summary
                );
            }
        }
        if !p.extensions.is_empty() {
            println!("    extensions: {:?}", p.extensions);
        }
        println!();
    }
    if shown == 0 && concern_filter.is_some() {
        eprintln!("(no positions matched --concern filter)");
    }
}

fn print_phase6(out: &Phase6Output, concern_filter: Option<&str>) {
    println!("Phase 6 (tensions) — cached {}", out.written_at);
    // Without the position table we can't easily filter by concern id
    // directly on a Tension. Skip the filter gracefully when provided.
    if concern_filter.is_some() {
        eprintln!("(note: tension --concern filter requires a loaded Atlas — skipped)");
    }
    for t in &out.tensions {
        println!("  {}: {} × {}", t.id, t.position_a_id, t.position_b_id);
        println!("    {}", t.description);
        if let Some(d) = &t.specific_disagreement {
            println!("    disagreement: {d}");
        }
        if let Some(s) = &t.structural_type {
            println!("    structural: {s}");
        }
        println!();
    }
}

fn print_phase7(out: &Phase7Output) {
    println!("Phase 7 (gaps) — cached {}", out.written_at);
    if out.gaps.is_empty() {
        println!("  (no gaps reported)");
        return;
    }
    for g in &out.gaps {
        println!("  {}: {}", g.id, g.gap_text);
        if !g.evidence.is_empty() {
            println!("    evidence: {}", g.evidence);
        }
        if !g.significance.is_empty() {
            println!("    significance: {}", g.significance);
        }
        println!();
    }
}

#[derive(Debug)]
enum Target {
    Phase1,
    Phase2,
    Phase3,
    Phase4,
    Phase5,
    Phase6,
    Phase7,
}

#[derive(Debug)]
struct ParsedShow {
    corpus_id: String,
    target: Target,
    chapter: Option<String>,
    concern: Option<String>,
}

fn parse_args(args: &[String]) -> Result<ParsedShow, String> {
    let mut corpus_id: Option<String> = None;
    let mut target: Option<Target> = None;
    let mut chapter: Option<String> = None;
    let mut concern: Option<String> = None;
    let mut i = 0;
    while i < args.len() {
        let a = &args[i];
        match a.as_str() {
            "--chapter" => {
                chapter = Some(
                    args.get(i + 1)
                        .ok_or("--chapter requires a value".to_string())?
                        .clone(),
                );
                i += 2;
            }
            "--concern" => {
                concern = Some(
                    args.get(i + 1)
                        .ok_or("--concern requires a value".to_string())?
                        .clone(),
                );
                i += 2;
            }
            other if other.starts_with("--") => {
                return Err(format!("unknown flag: {other}"));
            }
            other => {
                if corpus_id.is_none() {
                    corpus_id = Some(other.to_string());
                } else if target.is_none() {
                    target = Some(parse_target(other)?);
                } else {
                    return Err(format!("unexpected positional: {other}"));
                }
                i += 1;
            }
        }
    }

    let corpus_id = corpus_id.ok_or_else(|| "missing <corpus-id>".to_string())?;
    let target = target.ok_or_else(|| {
        "missing <target> (try phase1 | question-clusters | concerns | chunk-clusters | positions | tensions | gaps)"
            .to_string()
    })?;
    Ok(ParsedShow { corpus_id, target, chapter, concern })
}

fn parse_target(s: &str) -> Result<Target, String> {
    match s {
        "phase1" | "questions" | "extract" => Ok(Target::Phase1),
        "phase2" | "question-clusters" | "cluster-questions" => Ok(Target::Phase2),
        "phase3" | "concerns" | "name-concerns" => Ok(Target::Phase3),
        "phase4" | "chunk-clusters" | "cluster-chunks" => Ok(Target::Phase4),
        "phase5" | "positions" | "extract-positions" => Ok(Target::Phase5),
        "phase6" | "tensions" | "detect-tensions" => Ok(Target::Phase6),
        "phase7" | "gaps" | "detect-gaps" => Ok(Target::Phase7),
        other => Err(format!(
            "unknown target '{other}' — try phase1 | question-clusters | concerns | chunk-clusters | positions | tensions | gaps"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_show_args_accepts_phase1_alias() {
        let args = vec!["ak".into(), "phase1".into()];
        let p = parse_args(&args).unwrap();
        assert_eq!(p.corpus_id, "ak");
        assert!(matches!(p.target, Target::Phase1));
        assert!(p.concern.is_none());
    }

    #[test]
    fn parse_show_args_with_chapter_filter() {
        let args = vec![
            "ak".into(),
            "phase1".into(),
            "--chapter".into(),
            "sec_0001".into(),
        ];
        let p = parse_args(&args).unwrap();
        assert_eq!(p.chapter.as_deref(), Some("sec_0001"));
    }

    #[test]
    fn parse_show_args_rejects_unknown_target() {
        let err = parse_args(&["ak".into(), "mystery".into()]).unwrap_err();
        assert!(err.contains("unknown target"));
    }
}
