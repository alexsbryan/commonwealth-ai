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
use sovereign_contracts::tool_manifest::DeclaredTool;
use sovereign_contracts::types::{StepOutput, ToolContext};

// ── cluster-atlas ───────────────────────────────────────────

/// A parsed `cluster-atlas` invocation. Public so the `enrich build`
/// orchestrator constructs one directly instead of round-tripping
/// through argv.
#[derive(Debug, Clone)]
pub struct ParsedCluster {
    pub corpus_id: String,
}

/// What Phase 2 produced.
#[derive(Debug, Clone)]
pub struct ClusterReport {
    /// Cluster counts by facet, in `Facet::ALL` order. Only facets that
    /// produced at least one cluster appear.
    pub by_facet: Vec<(&'static str, usize)>,
    /// Sketches the clusterer could not place.
    pub noise: usize,
    pub run_path: std::path::PathBuf,
    pub cache_updated: bool,
}

impl ClusterReport {
    /// One line naming what this step produced, for the build
    /// orchestrator's `StepDone` event.
    pub fn summary(&self) -> String {
        let total: usize = self.by_facet.iter().map(|(_, n)| n).sum();
        if total == 0 {
            return format!("no clusters formed ({} sketch(es) noise)", self.noise);
        }
        let facets = self
            .by_facet
            .iter()
            .map(|(f, n)| format!("{n} {f}"))
            .collect::<Vec<_>>()
            .join(", ");
        format!("{total} cluster(s): {facets}; {} noise", self.noise)
    }
}

/// Run Phase 2 clustering. Pure of stdout: the operator's view is
/// [`render_cluster`]'s.
pub async fn run_cluster(parsed: &ParsedCluster) -> Result<ClusterReport, String> {
    let cfg = EnrichConfig::require(&parsed.corpus_id)
        .map_err(|e| format!("loading enrichment config: {e}"))?;
    let runner = build_runner(&cfg)?;

    let result = runner
        .phase_2_cluster_atlas()
        .await
        .map_err(|e| format!("phase 2 (atlas) failed: {e}"))?;

    let by_facet = Facet::ALL
        .iter()
        .filter_map(|&facet| {
            let count = result
                .output
                .clusters
                .iter()
                .filter(|c| c.facet == facet)
                .count();
            (count > 0).then_some((facet.as_str(), count))
        })
        .collect();

    Ok(ClusterReport {
        by_facet,
        noise: result.output.unclustered.len(),
        run_path: result.run_path,
        cache_updated: result.cache_updated,
    })
}

/// Print the per-facet coverage the way `svrn enrich cluster-atlas` always has.
pub fn render_cluster(corpus_id: &str, report: &ClusterReport) {
    println!("  running phase 2 (atlas) for {corpus_id}");
    for (facet, count) in &report.by_facet {
        println!("    · {count} cluster(s): {facet}");
    }
    if report.noise > 0 {
        println!("    · {} sketch(es) classified as noise", report.noise);
    }
    println!("  ✓ wrote {}", report.run_path.display());
    if report.cache_updated {
        println!("  ✓ cache updated");
    }
}

// ── name-atlas-clusters ─────────────────────────────────────

/// A parsed `name-atlas-clusters` invocation. Public so the `enrich
/// build` orchestrator constructs one directly instead of round-tripping
/// through argv.
#[derive(Debug, Clone)]
pub struct ParsedName {
    pub corpus_id: String,
}

/// Clusters that were named WITHOUT few-shot exemplars, and why.
///
/// The exemplar lookup used to be `(embed)(..).await.unwrap_or_default()`
/// followed by `if q.is_empty() { Vec::new() }` — so an embed ERROR and a
/// legitimately-empty vector both became "no exemplars", per cluster,
/// silently. The naming prompt then went out with no few-shot examples
/// and the run still reported success. That is the shape of note
/// `f4972e1b` (a dead embed slot degrades the stack while every surface
/// above it keeps answering), and ARCH §18.3 says absence is reported,
/// never defaulted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExemplarGap {
    /// The exemplar bank itself was empty — expected on a corpus with no
    /// banked exemplars, and NOT a fault.
    pub bank_empty: bool,
    /// The bank existed but could not be loaded; the run continued
    /// against an empty one. This IS a fault.
    pub bank_load_failed: bool,
    /// The embed call returned an error for this many clusters.
    pub embed_failed: usize,
    /// The embed call succeeded but returned an empty vector.
    pub embed_empty: usize,
}

impl ExemplarGap {
    /// Clusters named with no exemplars for a reason that is a fault.
    pub fn faulted(&self) -> usize {
        self.embed_failed + self.embed_empty
    }
}

/// What Phase 3 produced.
#[derive(Debug, Clone)]
pub struct NameReport {
    pub named: usize,
    pub total: usize,
    pub failures: Vec<PhaseFailure>,
    pub exemplars: ExemplarGap,
    pub run_path: std::path::PathBuf,
}

impl NameReport {
    /// One line naming what this step did, for the build orchestrator's
    /// `StepDone` event.
    pub fn summary(&self) -> String {
        let mut s = format!("{}/{} cluster(s) named", self.named, self.total);
        if !self.failures.is_empty() {
            let mut kinds: Vec<String> = self
                .failures
                .iter()
                .map(|f| format!("{:?}", f.kind))
                .collect();
            kinds.sort();
            kinds.dedup();
            s.push_str(&format!(
                ", {} failed ({})",
                self.failures.len(),
                kinds.join("/")
            ));
        }
        if self.exemplars.bank_load_failed {
            s.push_str("; exemplar bank failed to load — named without exemplars");
        } else if self.exemplars.bank_empty {
            s.push_str("; no exemplar bank");
        } else if self.exemplars.faulted() > 0 {
            s.push_str(&format!(
                "; {} named without exemplars ({} embed error(s), {} empty)",
                self.exemplars.faulted(),
                self.exemplars.embed_failed,
                self.exemplars.embed_empty
            ));
        }
        s
    }
}

/// Name every atlas cluster. Keeps its per-cluster progress printing —
/// each cluster is an LLM call and the line is the operator's only sign
/// the run is alive. The RESULT line is [`render_name`]'s.
pub async fn run_name(parsed: &ParsedName) -> Result<NameReport, String> {
    let corpus_id = parsed.corpus_id.clone();
    let cfg =
        EnrichConfig::require(&corpus_id).map_err(|e| format!("loading enrichment config: {e}"))?;
    let runner = build_runner(&cfg)?;

    // Read the Phase 1 + Phase 2 caches. Phase 1 gives us per-
    // section sketches to render into excerpts; Phase 2 gives the
    // cluster membership.
    let phase1: Phase1Output = match runner.cache().read(PipelinePhase::Questions) {
        Ok(Some(p)) => p,
        Ok(None) => {
            return Err(format!(
                "no Phase 1 cache — run `svrn enrich extract {corpus_id} --full` first"
            ))
        }
        Err(e) => return Err(format!("reading Phase 1 cache: {e}")),
    };
    let phase2: Phase2AtlasOutput = match runner.cache().read(PipelinePhase::AtlasClusters) {
        Ok(Some(p)) => p,
        Ok(None) => {
            return Err(format!(
                "no Phase 2 (atlas) cache — run `svrn enrich cluster-atlas {corpus_id}` first"
            ))
        }
        Err(e) => return Err(format!("reading Phase 2 (atlas) cache: {e}")),
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
    let (_, chat) = DaemonInferenceClient::from_enrich_config(&cfg)
        .map_err(|e| format!("building daemon client: {e}"))?
        .into_closures();

    // Load the exemplar bank for the naming phase. Empty is fine
    // — the prompt stands alone; exemplars steer but don't gate.
    let exemplar_path = paths::exemplars_dir(&cfg.corpus_id)
        .join(format!("{}.json", PipelinePhase::AtlasNamedClusters.id()));
    let (embed, _) = DaemonInferenceClient::from_enrich_config(&cfg)
        .map_err(|e| format!("building embed client: {e}"))?
        .into_closures();
    // A bank that fails to load is NOT the same as a corpus with no
    // banked exemplars, even though both leave the naming prompts
    // without few-shot examples. Recorded separately so the step's
    // summary can say which happened — until 2026-08-26 the failure was
    // a `warning:` on stderr and the run reported plain success. The
    // fallback also used to be `ExemplarBank::open(..).unwrap()`, which
    // turned a second failure into a panic.
    let mut bank_load_failed = false;
    let bank = match ExemplarBank::load_embedded(
        &exemplar_path,
        PipelinePhase::AtlasNamedClusters,
        &embed,
    )
    .await
    {
        Ok(b) => b,
        Err(e) => {
            bank_load_failed = true;
            eprintln!(
                "  · warning: could not load exemplar bank at {}: {} — continuing without",
                exemplar_path.display(),
                e
            );
            ExemplarBank::open(&exemplar_path, PipelinePhase::AtlasNamedClusters).map_err(
                |open_err| {
                    format!(
                        "exemplar bank at {} could not be loaded ({e}) and could not be \
                         opened empty either ({open_err})",
                        exemplar_path.display()
                    )
                },
            )?
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
    // Clusters named with no exemplars, split by cause — see `ExemplarGap`.
    let mut embed_failed: usize = 0;
    let mut embed_empty: usize = 0;
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
            match (embed)(&query_text).await {
                Ok(q) if !q.is_empty() => {
                    bank.select_top_k_facet(&q, 5, Some(cluster.facet.as_str()))
                }
                Ok(_) => {
                    // A successful call that returned nothing. Distinct from
                    // an error, and counted so the summary can say so.
                    embed_empty += 1;
                    tracing::warn!(
                        cluster = %cluster.id,
                        "phase 3 naming: embed returned an empty vector; this cluster is \
                         named without few-shot exemplars"
                    );
                    Vec::new()
                }
                Err(e) => {
                    embed_failed += 1;
                    tracing::warn!(
                        cluster = %cluster.id,
                        error = %e,
                        "phase 3 naming: embed failed; this cluster is named without \
                         few-shot exemplars"
                    );
                    Vec::new()
                }
            }
        };

        let Some(prompt) =
            pipeline.compose_phase3_facet(cluster, cluster.facet, &excerpts, &picked)
        else {
            return Err(format!(
                "pipeline `{}` does not implement compose_phase3_facet — use a *_atlas \
                 pipeline (e.g. literary_atlas)",
                pipeline.id()
            ));
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
    let run_path = runner
        .runs()
        .write(PipelinePhase::AtlasNamedClusters, "full", &output)
        .map_err(|e| format!("writing run file: {e}"))?;
    runner
        .cache()
        .write(PipelinePhase::AtlasNamedClusters, &output)
        .map_err(|e| format!("writing cache: {e}"))?;

    Ok(NameReport {
        named: output.named_clusters.len(),
        total,
        failures: output.failures,
        exemplars: ExemplarGap {
            bank_empty: bank.is_empty(),
            bank_load_failed,
            embed_failed,
            embed_empty,
        },
        run_path,
    })
}

/// Print the closing line the way `svrn enrich name-atlas-clusters` always has.
pub fn render_name(report: &NameReport) {
    println!(
        "  ✓ {} named cluster(s) — {}",
        report.named,
        report.run_path.display()
    );
}

// ── helpers ─────────────────────────────────────────────────

fn build_runner(cfg: &EnrichConfig) -> Result<PhaseRunner, String> {
    let registry = PipelineRegistry::builtin();
    let Some(pipeline) = super::pipeline_resolve::resolve_pipeline(cfg) else {
        return Err(format!(
            "unknown pipeline `{}`; known ids: {:?}",
            cfg.pipeline_id,
            registry.pipeline_ids()
        ));
    };

    let client = DaemonInferenceClient::from_enrich_config(cfg)
        .map_err(|e| format!("building daemon client: {e}"))?;
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
pub fn render_excerpts(
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
pub struct AtlasClusterTool;

impl AtlasClusterTool {
    /// Bind this tool's state to its `atlas_cluster` manifest row.
    ///
    /// The declared half — id, schema, permissions, retry — is the row in
    /// `tool-manifests/`. What is left here is the part that runs.
    pub fn declared(self) -> DeclaredTool {
        let state = Arc::new(self);
        sovereign_contracts::tool_manifest::declared("atlas_cluster", move |params, ctx| {
            let state = Arc::clone(&state);
            async move { state.run(&params, &ctx).await }
        })
    }

    /// The executable half of `atlas_cluster`.
    async fn run(
        &self,
        params: &serde_json::Value,
        _ctx: &ToolContext,
    ) -> sovereign_contracts::error::Result<StepOutput> {
        use sovereign_contracts::error::Error;

        let corpus = params
            .get("corpus")
            .and_then(|v| v.as_str())
            .ok_or_else(|| Error::Execution("atlas_cluster: missing required `corpus`".into()))?;

        // Calls the same `run_cluster` the CLI verb calls. This block used
        // to re-derive it — same config load, same `build_runner`, same
        // `phase_2_cluster_atlas` — because a `cmd_*` taking argv and
        // returning an exit code was not callable from here. It also had
        // to guess at failures ("exit 1 — is the daemon up?") because the
        // real cause went to stderr.
        let report = run_cluster(&ParsedCluster {
            corpus_id: corpus.to_string(),
        })
        .await
        .map_err(|e| Error::Execution(format!("atlas_cluster: {e}")))?;

        Ok(StepOutput::Text(format!(
            "atlas_cluster: {} → {}",
            report.summary(),
            report.run_path.display()
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sovereign_contracts::traits::Tool;

    /// The `atlas_cluster` leaf validates its `corpus` before any IO: missing or
    /// unknown corpus fails loudly. (The happy path needs the daemon + a resolved
    /// Phase-1 cache — exercised by the integration run, not a unit test.)
    #[tokio::test]
    async fn atlas_cluster_leaf_validates_corpus() {
        assert!(AtlasClusterTool
            .declared()
            .execute(&serde_json::json!({}), &ToolContext::default())
            .await
            .is_err());
        assert!(AtlasClusterTool
            .declared()
            .execute(
                &serde_json::json!({ "corpus": "definitely-not-a-real-corpus-zzz" }),
                &ToolContext::default()
            )
            .await
            .is_err());
    }
}
