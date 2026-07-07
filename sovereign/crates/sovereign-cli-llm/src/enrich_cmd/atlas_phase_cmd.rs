// SPDX-License-Identifier: AGPL-3.0-or-later
//! Driver for atlas-pipeline Phase 2 (clustering) and Phase 3
//! (facet-aware naming).
//!
//! Both subcommands follow the same shape as the v1 `phase_cmd.rs`
//! variants: load the enrichment config, build the daemon client,
//! dispatch to the runner. The naming path does the extra work of
//! walking every atlas cluster, rendering excerpts, calling chat,
//! parsing, and assembling a `Phase3AtlasOutput`.

use std::sync::Arc;

use corpus_engine::enrichment::pipeline::{
    atlas::SectionExtraction, AtlasCluster, ExemplarBank, Facet, NamedCluster, Phase1Output,
    Phase2AtlasOutput, Phase3AtlasOutput, PhaseFailure, PhaseFailureKind, PhaseRunner,
    PipelinePhase, PipelineRegistry, RunOutputWriter, SketchExcerpt,
};

use super::config::EnrichConfig;
use super::inference_client::DaemonInferenceClient;
use super::paths;
use async_trait::async_trait;
use sovereign_cli_shared::help::{self, Help, HelpSection};
use sovereign_core::traits::Tool;
use sovereign_core::types::{
    Effect, Idempotency, Latency, Permission, Scope, StepOutput, ToolContext, ToolDescriptor,
};

// ── cluster-atlas ───────────────────────────────────────────

const CLUSTER_HELP: Help = Help {
    command: "svrn enrich cluster-atlas",
    summary: "Phase 2 (atlas): cluster atlas sketches by facet.",
    sections: &[
        HelpSection::Usage("svrn enrich cluster-atlas <corpus-id>"),
        HelpSection::Notes(
            "Reads the Phase 1 cache (must carry section_extraction payloads; re-run \
             extract with literary_atlas if not) and writes atlas-clusters cache + run \
             file. Idempotent — re-running overwrites the cache in place.",
        ),
    ],
};

pub async fn cmd_cluster_atlas(args: &[String]) -> i32 {
    if help::wants_help(args) {
        help::print(&CLUSTER_HELP);
        return 0;
    }
    let Some(corpus_id) = args.first().cloned() else {
        eprintln!("error: missing <corpus-id>");
        eprintln!();
        help::print(&CLUSTER_HELP);
        return 2;
    };

    let cfg = match EnrichConfig::require(&corpus_id) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("error: loading enrichment config: {e}");
            return 1;
        }
    };

    let runner = match build_runner(&cfg) {
        Ok(r) => r,
        Err(rc) => return rc,
    };

    println!("  running phase 2 (atlas) for {}", corpus_id);
    let result = match runner.phase_2_cluster_atlas().await {
        Ok(r) => r,
        Err(e) => {
            eprintln!("error: phase 2 (atlas) failed: {e}");
            return 1;
        }
    };

    // Summarise per-facet counts so the operator sees coverage at
    // a glance.
    for &facet in Facet::ALL {
        let count = result
            .output
            .clusters
            .iter()
            .filter(|c| c.facet == facet)
            .count();
        if count > 0 {
            println!("    · {} cluster(s): {}", count, facet.as_str());
        }
    }
    let noise: usize = result.output.unclustered.len();
    if noise > 0 {
        println!("    · {} sketch(es) classified as noise", noise);
    }
    println!("  ✓ wrote {}", result.run_path.display());
    if result.cache_updated {
        println!("  ✓ cache updated");
    }
    0
}

// ── name-atlas-clusters ─────────────────────────────────────

const NAME_HELP: Help = Help {
    command: "svrn enrich name-atlas-clusters",
    summary:
        "Phase 3 (atlas): name each facet cluster with a position / trajectory / thread label.",
    sections: &[
        HelpSection::Usage("svrn enrich name-atlas-clusters <corpus-id>"),
        HelpSection::Notes(
            "Reads the Phase 2 (atlas) cache and calls the atlas pipeline's \
             compose_phase3_facet per cluster. Writes atlas-named-clusters cache + run \
             file. The pipeline must implement compose_phase3_facet (literary_atlas \
             does); pipelines returning None from the trait default get a clear \
             error here rather than silent empty output.",
        ),
    ],
};

pub async fn cmd_name_atlas_clusters(args: &[String]) -> i32 {
    if help::wants_help(args) {
        help::print(&NAME_HELP);
        return 0;
    }
    let Some(corpus_id) = args.first().cloned() else {
        eprintln!("error: missing <corpus-id>");
        eprintln!();
        help::print(&NAME_HELP);
        return 2;
    };

    let cfg = match EnrichConfig::require(&corpus_id) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("error: loading enrichment config: {e}");
            return 1;
        }
    };

    let runner = match build_runner(&cfg) {
        Ok(r) => r,
        Err(rc) => return rc,
    };

    // Read the Phase 1 + Phase 2 caches. Phase 1 gives us per-
    // section sketches to render into excerpts; Phase 2 gives the
    // cluster membership.
    let phase1: Phase1Output = match runner.cache().read(PipelinePhase::Questions) {
        Ok(Some(p)) => p,
        Ok(None) => {
            eprintln!(
                "error: no Phase 1 cache — run `svrn enrich extract {} --full` first",
                corpus_id
            );
            return 1;
        }
        Err(e) => {
            eprintln!("error: reading Phase 1 cache: {e}");
            return 1;
        }
    };
    let phase2: Phase2AtlasOutput = match runner.cache().read(PipelinePhase::AtlasClusters) {
        Ok(Some(p)) => p,
        Ok(None) => {
            eprintln!(
                "error: no Phase 2 (atlas) cache — run `svrn enrich cluster-atlas \
                 {}` first",
                corpus_id
            );
            return 1;
        }
        Err(e) => {
            eprintln!("error: reading Phase 2 (atlas) cache: {e}");
            return 1;
        }
    };

    // Index sections by id so excerpt rendering is O(1) per ref.
    let sections: std::collections::HashMap<String, SectionExtraction> = phase1
        .questions_by_chapter
        .into_iter()
        .filter_map(|c| c.section_extraction.map(|se| (c.chapter_id, se)))
        .collect();

    // Build daemon closures ourselves — the runner's private chat
    // closure is the v2 chat; name-atlas-clusters drives it
    // directly without re-entering phase_*_compose_phase3.
    let (_, chat) = match DaemonInferenceClient::from_enrich_config(&cfg) {
        Ok(c) => c.into_closures(),
        Err(e) => {
            eprintln!("error: building daemon client: {e}");
            return 1;
        }
    };

    // Load the exemplar bank for the naming phase. Empty is fine
    // — the prompt stands alone; exemplars steer but don't gate.
    let exemplar_path = paths::exemplars_dir(&cfg.corpus_id)
        .join(format!("{}.json", PipelinePhase::AtlasNamedClusters.id()));
    let (embed, _) = match DaemonInferenceClient::from_enrich_config(&cfg) {
        Ok(c) => c.into_closures(),
        Err(e) => {
            eprintln!("error: building embed client: {e}");
            return 1;
        }
    };
    let bank = match ExemplarBank::load_embedded(
        &exemplar_path,
        PipelinePhase::AtlasNamedClusters,
        &embed,
    )
    .await
    {
        Ok(b) => b,
        Err(e) => {
            eprintln!(
                "  · warning: could not load exemplar bank at {}: {} — continuing without",
                exemplar_path.display(),
                e
            );
            ExemplarBank::open(&exemplar_path, PipelinePhase::AtlasNamedClusters).unwrap()
        }
    };

    println!(
        "  running phase 3 (atlas) — naming {} cluster(s) across {} facet(s)",
        phase2.clusters.len(),
        Facet::ALL
            .iter()
            .filter(|f| phase2.clusters.iter().any(|c| c.facet == **f))
            .count()
    );

    let pipeline = runner.pipeline().clone();
    let mut named: Vec<NamedCluster> = Vec::with_capacity(phase2.clusters.len());
    // Per-cluster failures land here rather than either returning
    // early (stop the world on one bad cluster) or swallowing via
    // `continue` (what the pre-Landing-3.A code did, which lost the
    // signal entirely). Each record carries enough context for the
    // aggregator to route the operator to the exact remediation.
    let mut failures: Vec<PhaseFailure> = Vec::new();
    let total = phase2.clusters.len();

    for (i, cluster) in phase2.clusters.iter().enumerate() {
        print!(
            "    [{}/{}] {} ({})… ",
            i + 1,
            total,
            cluster.id,
            cluster.facet.as_str()
        );
        use std::io::Write;
        std::io::stdout().flush().ok();

        let excerpts = render_excerpts(cluster, &sections);
        // Defensive: if the cluster's refs don't resolve to any
        // sketches we can render, skip rather than send an empty
        // prompt to the LLM (which then either echoes the schema
        // template or refuses outright — both end up as Phase 3
        // failures that look like model-quality issues but are
        // actually data plumbing). A non-empty refs list with empty
        // excerpts means upstream id corruption — record as Skipped
        // so the operator sees the signal instead of a parse error.
        if excerpts.is_empty() {
            println!(
                "SKIP: cluster has no resolvable sketches ({} ref(s) but none looked up — likely upstream id mismatch)",
                cluster.refs.len()
            );
            failures.push(PhaseFailure {
                phase: PipelinePhase::AtlasNamedClusters,
                subject: format!("cluster:{}:{}", cluster.facet.as_str(), cluster.id),
                kind: PhaseFailureKind::Skipped,
                reason: format!(
                    "cluster {} has {} ref(s) but none resolved to sketches in the section map — \
                     check Phase 1 section_id integrity",
                    cluster.id,
                    cluster.refs.len()
                ),
                raw_response_head: None,
            });
            continue;
        }
        let query_text = excerpts
            .iter()
            .map(|e| e.content.as_str())
            .collect::<Vec<_>>()
            .join(" / ");
        let picked: Vec<&_> = if bank.is_empty() {
            Vec::new()
        } else {
            let q: Vec<f32> = (embed)(&query_text).await.unwrap_or_default();
            if q.is_empty() {
                Vec::new()
            } else {
                bank.select_top_k_facet(&q, 5, Some(cluster.facet.as_str()))
            }
        };

        let Some(prompt) =
            pipeline.compose_phase3_facet(cluster, cluster.facet, &excerpts, &picked)
        else {
            eprintln!(
                "FAILED: pipeline `{}` does not implement compose_phase3_facet — use \
                 a *_atlas pipeline (e.g. literary_atlas)",
                pipeline.id()
            );
            return 1;
        };
        let subject = format!("cluster:{}:{}", cluster.facet.as_str(), cluster.id);
        let response = match (chat)(&prompt).await {
            Ok(r) => r,
            Err(e) => {
                println!("FAILED: chat error: {e}");
                failures.push(PhaseFailure {
                    phase: PipelinePhase::AtlasNamedClusters,
                    subject: subject.clone(),
                    kind: PhaseFailureKind::ChatError,
                    reason: format!("chat error naming cluster {}: {e}", cluster.id),
                    raw_response_head: None,
                });
                continue;
            }
        };
        let parsed = match pipeline.parse_phase3_facet(cluster.facet, &response) {
            Ok(p) => p,
            Err(e) => {
                println!("FAILED: parse error: {e}");
                // Keep the first ~1 KiB of the response so the
                // operator can see what the model actually produced
                // — classifier improvements land in a follow-up;
                // for now ParseDrift is the correct default since
                // the terse-retry path hasn't been wired for Phase 3.
                let head = truncate_chars(&response, 1024);
                failures.push(PhaseFailure {
                    phase: PipelinePhase::AtlasNamedClusters,
                    subject: subject.clone(),
                    kind: PhaseFailureKind::ParseDrift,
                    reason: format!("parse error naming cluster {}: {e}", cluster.id),
                    raw_response_head: Some(head),
                });
                continue;
            }
        };
        if parsed.label.trim().is_empty() {
            // Well-formed envelope, empty label — the cluster keeps
            // its id but loses a human-readable name. Surface it
            // separately so the aggregator's remediation points at
            // the prompt rather than at a plain retry.
            println!("FAILED: empty label");
            failures.push(PhaseFailure {
                phase: PipelinePhase::AtlasNamedClusters,
                subject: subject.clone(),
                kind: PhaseFailureKind::ClusterNamingFailed,
                reason: format!(
                    "cluster {} parsed cleanly but produced an empty label",
                    cluster.id
                ),
                raw_response_head: Some(truncate_chars(&response, 1024)),
            });
            continue;
        }
        println!("{}", one_line(&parsed.label));
        named.push(NamedCluster {
            id: format!("ncl_{:04}", named.len() + 1),
            cluster_id: cluster.id.clone(),
            facet: cluster.facet,
            label: parsed.label,
            metadata: parsed.metadata,
        });
    }

    let output = Phase3AtlasOutput {
        schema_version: Phase3AtlasOutput::SCHEMA_VERSION,
        pipeline_id: cfg.pipeline_id.clone(),
        named_clusters: named,
        failures,
        written_at: chrono::Utc::now().to_rfc3339(),
    };
    let run_path = match runner
        .runs()
        .write(PipelinePhase::AtlasNamedClusters, "full", &output)
    {
        Ok(p) => p,
        Err(e) => {
            eprintln!("error: writing run file: {e}");
            return 1;
        }
    };
    if let Err(e) = runner
        .cache()
        .write(PipelinePhase::AtlasNamedClusters, &output)
    {
        eprintln!("error: writing cache: {e}");
        return 1;
    }
    println!(
        "  ✓ {} named cluster(s) — {}",
        output.named_clusters.len(),
        run_path.display()
    );
    0
}

// ── helpers ─────────────────────────────────────────────────

fn build_runner(cfg: &EnrichConfig) -> Result<PhaseRunner, i32> {
    let registry = PipelineRegistry::builtin();
    let Some(pipeline) = super::pipeline_resolve::resolve_pipeline(cfg) else {
        eprintln!(
            "error: unknown pipeline `{}`; known ids: {:?}",
            cfg.pipeline_id,
            registry.pipeline_ids()
        );
        return Err(1);
    };

    let client = match DaemonInferenceClient::from_enrich_config(cfg) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("error: building daemon client: {e}");
            return Err(1);
        }
    };
    let (embed, chat) = client.into_closures();

    let cache = cfg.phase_cache();
    let runs = RunOutputWriter::new(paths::runs_dir(&cfg.corpus_id));
    Ok(PhaseRunner::new(
        pipeline,
        embed,
        chat,
        cache,
        runs,
        paths::exemplars_dir(&cfg.corpus_id),
    ))
}

/// Render a cluster's refs into per-facet `SketchExcerpt`s by indexing the
/// section map (`chapter_id → SectionExtraction`). Reused verbatim by the
/// `atlas_cluster_excerpts` workflow primitive so a workflow-composed name phase
/// builds the same excerpts the bespoke facet-naming loop does.
pub(crate) fn render_excerpts(
    cluster: &AtlasCluster,
    sections: &std::collections::HashMap<String, SectionExtraction>,
) -> Vec<SketchExcerpt> {
    let mut out = Vec::with_capacity(cluster.refs.len());
    for r in &cluster.refs {
        let Some(section) = sections.get(&r.section_id) else {
            continue;
        };
        match cluster.facet {
            Facet::Question => {
                if let Some(q) = section.questions_raised.get(r.sketch_index) {
                    out.push(SketchExcerpt {
                        section_id: r.section_id.clone(),
                        content: q.content.clone(),
                        anchor: q.anchor.clone(),
                    });
                }
            }
            Facet::Claim => {
                if let Some(c) = section.claims.get(r.sketch_index) {
                    let prefix = match &c.attributed_to {
                        Some(a) if !a.is_empty() => format!("{}: ", a),
                        _ => String::new(),
                    };
                    out.push(SketchExcerpt {
                        section_id: r.section_id.clone(),
                        content: format!(
                            "{}[{}/{}] {}",
                            prefix,
                            c.discourse_act.as_str_repr(),
                            c.epistemic_status.as_str_repr(),
                            c.content
                        ),
                        anchor: c.anchor.clone(),
                    });
                }
            }
            Facet::EntityState => {
                if let Some(s) = section.entities_developed.get(r.sketch_index) {
                    out.push(SketchExcerpt {
                        section_id: r.section_id.clone(),
                        content: format!("{}: {}", s.entity_name, s.label),
                        anchor: s.anchor.clone(),
                    });
                }
            }
            Facet::RelationState => {
                if let Some(s) = section.relations_developed.get(r.sketch_index) {
                    out.push(SketchExcerpt {
                        section_id: r.section_id.clone(),
                        content: format!("{}: {}", s.participants.join(" × "), s.label),
                        anchor: s.anchor.clone(),
                    });
                }
            }
            Facet::Event => {
                if let Some(e) = section.events.get(r.sketch_index) {
                    out.push(SketchExcerpt {
                        section_id: r.section_id.clone(),
                        content: e.description.clone(),
                        anchor: e.anchor.clone(),
                    });
                }
            }
        }
    }
    out
}

fn one_line(s: &str) -> String {
    let flat: String = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= 80 {
        flat
    } else {
        flat.chars().take(77).collect::<String>() + "…"
    }
}

/// UTF-8 safe char-cap that appends a `… [+N chars]` marker when
/// truncating. Mirrors the Phase-1 response-head policy so the
/// aggregator sees consistent shape across phases.
fn truncate_chars(s: &str, cap: usize) -> String {
    let total = s.chars().count();
    if total <= cap {
        return s.to_string();
    }
    let head: String = s.chars().take(cap).collect();
    format!("{head}… [+{} chars]", total - cap)
}

// Silence unused imports if the sketch types aren't referenced in
// this file directly (they're used via `sections.get(...)`).
#[allow(dead_code)]
fn _compile_time_reexport_guard() {
    let _ = Arc::new(());
}

/// `atlas_cluster` — literary-atlas **Phase 2** as a workflow leaf: cluster the
/// Phase-1 section sketches into typed facet clusters (HDBSCAN per facet over
/// sketch embeddings).
///
/// One atomic op wrapping the *exact* bespoke `PhaseRunner::phase_2_cluster_atlas`
/// — so a workflow-built clustering is faithful to `svrn enrich
/// cluster-atlas`. It reuses the same `build_runner` (which wires the daemon
/// embed closure), which is why this leaf lives here in `enrich_cmd` rather than
/// in `sovereign-tools`. Reads cache/questions.json, writes cache/atlas-clusters.json.
/// Effect `Write`; needs the daemon up (per-sketch embeddings).
pub(crate) struct AtlasClusterTool;

#[async_trait]
impl Tool for AtlasClusterTool {
    fn descriptor(&self) -> ToolDescriptor {
        ToolDescriptor {
            id: "atlas_cluster".to_string(),
            name: "atlas_cluster".to_string(),
            description:
                "Literary-atlas Phase 2: cluster Phase-1 section sketches into typed \
                          facet clusters (HDBSCAN per facet over sketch embeddings). Reads \
                          cache/questions.json, writes cache/atlas-clusters.json. Needs the daemon."
                    .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "corpus": { "type": "string", "description": "Corpus id (must be enrich-init'd with an *_atlas pipeline)" }
                },
                "required": ["corpus"]
            }),
            examples: vec![],
            effect: Effect::Write,
            idempotency: Idempotency::Idempotent,
            latency: Latency::Slow,
            scope: Scope::Persistent,
            output_schema: None,
        }
    }

    fn required_permissions(&self) -> Vec<Permission> {
        vec![]
    }

    async fn execute(
        &self,
        params: &serde_json::Value,
        _ctx: &ToolContext,
    ) -> sovereign_core::error::Result<StepOutput> {
        use sovereign_core::error::Error;

        let corpus = params
            .get("corpus")
            .and_then(|v| v.as_str())
            .ok_or_else(|| Error::Execution("atlas_cluster: missing required `corpus`".into()))?;

        let cfg = EnrichConfig::require(corpus).map_err(|e| {
            Error::Execution(format!(
                "atlas_cluster: enrichment config for `{corpus}`: {e} — run `enrich init` first"
            ))
        })?;
        // `build_runner` wires the daemon embed closure + caches; it prints the
        // specific cause to stderr and returns an exit code, which we surface as
        // a generic error (the common cause — missing config — is already caught
        // above with a clear message).
        let runner = build_runner(&cfg).map_err(|rc| {
            Error::Execution(format!(
                "atlas_cluster: could not build the phase runner (exit {rc}) — is the daemon up?"
            ))
        })?;
        let result = runner
            .phase_2_cluster_atlas()
            .await
            .map_err(|e| Error::Execution(format!("atlas_cluster: phase 2 failed: {e}")))?;

        let clusters = result.output.clusters.len();
        let noise = result.output.unclustered.len();
        Ok(StepOutput::Text(format!(
            "atlas_cluster: {clusters} facet cluster(s), {noise} noise sketch(es) → {}",
            result.run_path.display()
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> ToolContext {
        ToolContext {
            conversation_id: Default::default(),
            task_id: None,
            working_directory: None,
            in_reasoning_loop: false,
            agent_session_token: None,
            turn_index: 0,
        }
    }

    /// The `atlas_cluster` leaf validates its `corpus` before any IO: missing or
    /// unknown corpus fails loudly. (The happy path needs the daemon + a resolved
    /// Phase-1 cache — exercised by the integration run, not a unit test.)
    #[tokio::test]
    async fn atlas_cluster_leaf_validates_corpus() {
        assert!(AtlasClusterTool
            .execute(&serde_json::json!({}), &ctx())
            .await
            .is_err());
        assert!(AtlasClusterTool
            .execute(
                &serde_json::json!({ "corpus": "definitely-not-a-real-corpus-zzz" }),
                &ctx()
            )
            .await
            .is_err());
    }
}
