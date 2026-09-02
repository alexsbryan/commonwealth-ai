// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn enrich atlas-configuration` — Phase A Step 5 (Phase 8).
//!
//! Reads the resolved atlas (atoms.json + edges.json),
//! summarises it for the pipeline's Phase 8 prompt, dispatches the
//! LLM, and writes the parsed Configuration atoms to
//! `atlas/configurations.json`. Opt-in per pipeline — a pipeline
//! that returns `false` from `Pipeline::runs_configuration_phase()`
//! is skipped with a clear message rather than silently writing
//! an empty file.
//!
//! Configurations are also merged into `atoms.json` via a
//! targeted rewrite, so downstream traversal queries that load the
//! whole atom set see them without reading a second file.

use std::path::PathBuf;

use corpus_engine::enrichment::atlas::{
    analysis::configuration::{
        parse_configurations, summarise_atlas, AtlasSummary, AtlasSummaryParams,
        ConfigurationsOutput, Phase8ParseItem,
    },
    read_atlas_atoms, read_atlas_edges, write_atlas_configurations, write_atlas_full, AtomEnvelope,
    Configuration, ATLAS_DIRNAME,
};
use corpus_engine::enrichment::pipeline::ChatPrompt;

use super::config::EnrichConfig;
use super::inference_client::DaemonInferenceClient;
use super::paths;

/// Why Phase 8 stopped.
///
/// A pipeline that does not opt into configuration detection exited `2`
/// and a genuine failure exited `1`; both reached the orchestrator as
/// "nonzero" with no way to tell a misconfigured corpus from a broken
/// run (ARCH §18.3).
#[derive(Debug, Clone)]
pub enum ConfigureError {
    /// The corpus's pipeline does not run Phase 8 at all. A usage
    /// error, not a failure.
    NotOptedIn { pipeline_id: String },
    /// Phase 8 was attempted and failed.
    Failed(String),
}

impl ConfigureError {
    /// The code `svrn enrich atlas-configuration` exits with.
    pub fn exit_code(&self) -> i32 {
        match self {
            Self::NotOptedIn { .. } => 2,
            Self::Failed(_) => 1,
        }
    }

    pub fn message(&self) -> String {
        match self {
            Self::NotOptedIn { pipeline_id } => format!(
                "pipeline `{pipeline_id}` does not opt into Phase 8 (configuration detection). Switch to a pipeline that returns true from `runs_configuration_phase()` (e.g. `literary_atlas`) or skip this step."
            ),
            Self::Failed(m) => m.clone(),
        }
    }
}

/// What Phase 8 produced.
#[derive(Debug, Clone)]
pub struct ConfigurationReport {
    pub configurations: usize,
    /// Items the model emitted, before invented-reference filtering.
    pub items_returned: usize,
}

impl ConfigurationReport {
    /// One line naming what this step produced, for the build
    /// orchestrator's `StepDone` event.
    pub fn summary(&self) -> String {
        let dropped = self.items_returned.saturating_sub(self.configurations);
        if dropped == 0 {
            format!("{} configuration(s)", self.configurations)
        } else {
            format!(
                "{} configuration(s) ({dropped} of {} dropped for invented atom references)",
                self.configurations, self.items_returned
            )
        }
    }
}

/// Run Phase 8 configuration detection. Keeps its progress printing —
/// the pass makes a chat call.
pub async fn run(parsed: &ParsedConfig) -> Result<ConfigurationReport, ConfigureError> {
    let cfg = EnrichConfig::require(&parsed.corpus_id)
        .map_err(|e| ConfigureError::Failed(format!("loading enrichment config: {e}")))?;

    // Resolve the pipeline and check the opt-in gate.
    let Some(pipeline) = super::pipeline_resolve::resolve_pipeline(&cfg) else {
        return Err(ConfigureError::Failed(format!(
            "unknown pipeline `{}` in enrichment config",
            cfg.pipeline_id
        )));
    };
    if !pipeline.runs_configuration_phase() {
        return Err(ConfigureError::NotOptedIn {
            pipeline_id: cfg.pipeline_id.clone(),
        });
    }

    // Read the resolved atlas.
    // Read + partition the atlas through the ONE helper that does it.
    // Until 2026-08-26 this function inlined a byte-identical copy of
    // `build_atlas_summary`'s body — same nine partition vectors, same
    // `summarise_atlas` call with the same params — because the workflow
    // primitive could not call a `cmd_*` that took argv and returned an
    // exit code, so it grew its own. One decider, one name (ARCH §10.6).
    let summary = build_atlas_summary(&cfg.corpus_id).map_err(ConfigureError::Failed)?;

    println!(
        "  loaded atlas: {} entity / {} relation / {} claim / {} question / {} event synopsis(es), {} section(s)",
        summary.entities.len(),
        summary.relations.len(),
        summary.top_claims.len(),
        summary.open_questions.len(),
        summary.key_events.len(),
        summary.section_count,
    );

    // Build + dispatch the Phase 8 prompt.
    let Some(prompt): Option<ChatPrompt> = pipeline.compose_phase8_configuration(&summary, &[])
    else {
        return Err(ConfigureError::Failed(format!(
            "pipeline `{}` returned None from compose_phase8_configuration despite opting into the phase. This is a pipeline implementation bug.",
            cfg.pipeline_id
        )));
    };

    let client = DaemonInferenceClient::from_enrich_config(&cfg)
        .map_err(|e| ConfigureError::Failed(format!("building daemon client: {e}")))?;
    let (_embed, chat) = client.into_closures();

    println!("  dispatching Phase 8 configuration prompt…");
    let response = chat(&prompt)
        .await
        .map_err(|e| ConfigureError::Failed(format!("Phase 8 chat failed: {e}")))?;

    let items = pipeline
        .parse_phase8_configuration(&response)
        .map_err(|e| {
            ConfigureError::Failed(format!(
                "Phase 8 parse failed: {e}. Response head:\n{}",
                response.chars().take(800).collect::<String>()
            ))
        })?;

    // Merge through the ONE helper that does it. This function used to
    // inline a second copy of `finalize_configurations` — the same
    // known-atom-id set, the same `parse_configurations`, the same
    // trajectory read and `write_atlas_full`. The workflow primitive
    // already called the helper; the CLI path re-derived it.
    let items_returned = items.len();
    let configurations =
        finalize_configurations(&cfg.corpus_id, items).map_err(ConfigureError::Failed)?;

    println!("  ✓ {} configuration(s) produced", configurations.len());
    for c in &configurations {
        println!(
            "    · [{}] {} (confidence {:.2}) — {} constituent atom(s)",
            c.id.as_str(),
            c.label,
            c.confidence,
            c.constituent_atoms.len()
        );
    }

    Ok(ConfigurationReport {
        configurations: configurations.len(),
        items_returned,
    })
}

fn atlas_dir_for(corpus_id: &str) -> PathBuf {
    paths::index_root(corpus_id).join(ATLAS_DIRNAME)
}

/// Load the resolved atlas (`atoms.json` + `edges.json`) and summarise it for the
/// Phase 8 prompt — the LOAD half of `cmd_atlas_configuration`, exposed so the
/// workflow `atlas_summary` primitive reaches the SAME `AtlasSummary` the bespoke
/// cmd builds (reusing `summarise_atlas` verbatim). No divergence.
pub fn build_atlas_summary(corpus_id: &str) -> Result<AtlasSummary, String> {
    let atlas_dir = atlas_dir_for(corpus_id);
    let atoms_file = read_atlas_atoms(&atlas_dir).map_err(|e| {
        format!(
            "reading {}/atoms.json: {e} — run atlas-resolve first",
            atlas_dir.display()
        )
    })?;
    let edges_file = read_atlas_edges(&atlas_dir).map_err(|e| {
        format!(
            "reading {}/edges.json: {e} — run atlas-resolve first",
            atlas_dir.display()
        )
    })?;

    let mut entities = Vec::new();
    let mut events = Vec::new();
    let mut states = Vec::new();
    let mut relations = Vec::new();
    let mut claims = Vec::new();
    let mut questions = Vec::new();
    for a in atoms_file.atoms {
        match a {
            AtomEnvelope::Entity(x) => entities.push(x),
            AtomEnvelope::Event(x) => events.push(x),
            AtomEnvelope::State(x) => states.push(x),
            AtomEnvelope::Relation(x) => relations.push(x),
            AtomEnvelope::Claim(x) => claims.push(x),
            AtomEnvelope::Question(x) => questions.push(x),
            _ => {}
        }
    }

    // section_count proxy, identical to the bespoke cmd.
    let mut sections: std::collections::HashSet<String> = std::collections::HashSet::new();
    for e in &events {
        sections.insert(e.section_position.section_id.clone());
    }
    for s in &states {
        sections.insert(s.section_range.start.clone());
    }
    for ent in &entities {
        sections.insert(ent.first_appearance.chunk_id.clone());
    }

    Ok(summarise_atlas(
        &entities,
        &events,
        &states,
        &relations,
        &claims,
        &questions,
        &edges_file.edges,
        sections.len(),
        AtlasSummaryParams::default(),
    ))
}

/// Validate the Phase 8 parse items against the atlas and merge the resulting
/// Configuration atoms into `configurations.json` + `atoms.json` — the WRITE half
/// of `cmd_atlas_configuration`, exposed so the workflow `atlas_write_configurations`
/// primitive finalizes identically (reusing `parse_configurations` + `write_atlas_full`
/// verbatim). Returns the configuration count.
pub fn finalize_configurations(
    corpus_id: &str,
    items: Vec<Phase8ParseItem>,
) -> Result<Vec<Configuration>, String> {
    let atlas_dir = atlas_dir_for(corpus_id);
    let atoms_file = read_atlas_atoms(&atlas_dir)
        .map_err(|e| format!("reading {}/atoms.json: {e}", atlas_dir.display()))?;
    let edges_file = read_atlas_edges(&atlas_dir)
        .map_err(|e| format!("reading {}/edges.json: {e}", atlas_dir.display()))?;

    let mut entities = Vec::new();
    let mut events = Vec::new();
    let mut states = Vec::new();
    let mut relations = Vec::new();
    let mut claims = Vec::new();
    let mut questions = Vec::new();
    let mut argument_reconstructions = Vec::new();
    let mut positions = Vec::new();
    let mut oppositions = Vec::new();
    for a in atoms_file.atoms {
        match a {
            AtomEnvelope::Entity(x) => entities.push(x),
            AtomEnvelope::Event(x) => events.push(x),
            AtomEnvelope::State(x) => states.push(x),
            AtomEnvelope::Relation(x) => relations.push(x),
            AtomEnvelope::Claim(x) => claims.push(x),
            AtomEnvelope::Question(x) => questions.push(x),
            // Drop prior Phase 8 output; this pass replaces it.
            AtomEnvelope::Configuration(_) => {}
            AtomEnvelope::ArgumentReconstruction(x) => argument_reconstructions.push(x),
            AtomEnvelope::Position(x) => positions.push(x),
            AtomEnvelope::Opposition(x) => oppositions.push(x),
            // Asset substrate is preserved by the writer, not here (matches the
            // bespoke cmd_atlas_configuration partition).
            AtomEnvelope::Asset(_) => {}
        }
    }

    // Known atom ids so parse_configurations drops invented references.
    let mut known_atom_ids: std::collections::HashSet<String> = std::collections::HashSet::new();
    for e in &entities {
        known_atom_ids.insert(e.id.as_str().to_string());
    }
    for e in &events {
        known_atom_ids.insert(e.id.as_str().to_string());
    }
    for s in &states {
        known_atom_ids.insert(s.id.as_str().to_string());
    }
    for r in &relations {
        known_atom_ids.insert(r.id.as_str().to_string());
    }
    for c in &claims {
        known_atom_ids.insert(c.id.as_str().to_string());
    }
    for q in &questions {
        known_atom_ids.insert(q.id.as_str().to_string());
    }

    let configurations = parse_configurations(items, &known_atom_ids);

    let out = ConfigurationsOutput::new(configurations.clone());
    write_atlas_configurations(&atlas_dir, &out)
        .map_err(|e| format!("writing configurations.json: {e}"))?;

    // Preserve trajectories across the atoms.json rewrite.
    let trajectories_path = atlas_dir.join("trajectories.json");
    let trajectories: std::collections::BTreeMap<
        String,
        corpus_engine::enrichment::atlas::resolution::Trajectory,
    > = match std::fs::read(&trajectories_path) {
        Ok(bytes) => {
            match serde_json::from_slice::<corpus_engine::enrichment::atlas::TrajectoriesFile>(
                &bytes,
            ) {
                Ok(file) => serde_json::from_value(file.trajectories).unwrap_or_default(),
                Err(_) => std::collections::BTreeMap::new(),
            }
        }
        Err(_) => std::collections::BTreeMap::new(),
    };

    write_atlas_full(
        &atlas_dir,
        &entities,
        &events,
        &states,
        &relations,
        &claims,
        &questions,
        &configurations,
        &argument_reconstructions,
        &positions,
        &oppositions,
        &edges_file.edges,
        &trajectories,
    )
    .map_err(|e| format!("rewriting atoms.json: {e}"))?;

    Ok(configurations)
}

/// A parsed `atlas-configuration` invocation. Public so the `enrich
/// build` orchestrator constructs one directly instead of round-tripping
/// through argv.
#[derive(Debug, Clone)]
pub struct ParsedConfig {
    pub corpus_id: String,
}

pub fn parse_args(args: &[String]) -> Result<ParsedConfig, String> {
    let mut corpus_id: Option<String> = None;
    for arg in args {
        match arg.as_str() {
            other if other.starts_with("--") => {
                return Err(format!("unknown flag: {other}"));
            }
            other => {
                if corpus_id.is_none() {
                    corpus_id = Some(other.to_string());
                } else {
                    return Err(format!("unexpected positional argument: {other}"));
                }
            }
        }
    }
    let corpus_id = corpus_id.ok_or_else(|| "missing <corpus-id>".to_string())?;
    Ok(ParsedConfig { corpus_id })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_args_accepts_bare_corpus_id() {
        let p = parse_args(&["brothers_karamazov".into()]).unwrap();
        assert_eq!(p.corpus_id, "brothers_karamazov");
    }

    #[test]
    fn parse_args_rejects_missing_corpus_id() {
        let err = parse_args(&[]).unwrap_err();
        assert!(err.contains("corpus-id"));
    }

    #[test]
    fn parse_args_rejects_unknown_flag() {
        let err = parse_args(&["bk".into(), "--nope".into()]).unwrap_err();
        assert!(err.contains("unknown flag"));
    }
}
