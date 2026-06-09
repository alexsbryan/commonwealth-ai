// SPDX-License-Identifier: AGPL-3.0-or-later
//! `sovereign enrich atlas-configuration` — Phase A Step 5 (Phase 8).
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
        parse_configurations, summarise_atlas, AtlasSummaryParams, ConfigurationsOutput,
    },
    read_atlas_atoms, read_atlas_edges, write_atlas_configurations, write_atlas_full, AtomEnvelope,
    ATLAS_DIRNAME,
};
use corpus_engine::enrichment::pipeline::{ChatPrompt, PipelineRegistry};

use super::config::EnrichConfig;
use super::inference_client::DaemonInferenceClient;
use super::paths;
use sovereign_cli_shared::help::{self, Help, HelpSection};

const HELP: Help = Help {
    command: "sovereign enrich atlas-configuration",
    summary: "Detect 0–3 interpretive Configuration atoms from the resolved atlas (LLM).",
    sections: &[
        HelpSection::Usage("sovereign enrich atlas-configuration <corpus-id>"),
        HelpSection::Examples(&[(
            "sovereign enrich atlas-configuration brothers_karamazov",
            "Summarise atlas → prompt the configured pipeline's Phase 8 → write configurations.json.",
        )]),
        HelpSection::Notes(
            "Requires a prior `sovereign enrich atlas-resolve <corpus> --phase all`. \
             Opt-in: only pipelines whose `runs_configuration_phase()` returns true \
             (`literary_atlas`, future `philosophy_atlas`) actually dispatch an LLM call. \
             Produces `~/.sovereign/indexes/<corpus>/atlas/configurations.json` and \
             merges configurations into `atoms.json` so the brief assembler sees them \
             without a separate read.",
        ),
    ],
};

pub async fn cmd_atlas_configuration(args: &[String]) -> i32 {
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

    let cfg = match EnrichConfig::require(&parsed.corpus_id) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("error: loading enrichment config: {e}");
            return 1;
        }
    };

    // Resolve the pipeline and check the opt-in gate.
    let registry = PipelineRegistry::builtin();
    let Some(pipeline) = registry.get(&cfg.pipeline_id) else {
        eprintln!(
            "error: unknown pipeline `{}` in enrichment config",
            cfg.pipeline_id
        );
        return 1;
    };
    if !pipeline.runs_configuration_phase() {
        eprintln!(
            "error: pipeline `{}` does not opt into Phase 8 (configuration detection). \
             Switch to a pipeline that returns true from `runs_configuration_phase()` \
             (e.g. `literary_atlas`) or skip this step.",
            cfg.pipeline_id
        );
        return 2;
    }

    // Read the resolved atlas.
    let atlas_dir = atlas_dir_for(&cfg.corpus_id);
    let atoms_file = match read_atlas_atoms(&atlas_dir) {
        Ok(a) => a,
        Err(e) => {
            eprintln!(
                "error: reading {}/atoms.json: {e}. Run `sovereign enrich atlas-resolve \
                 {} --phase all` first.",
                atlas_dir.display(),
                cfg.corpus_id
            );
            return 1;
        }
    };
    let edges_file = match read_atlas_edges(&atlas_dir) {
        Ok(e) => e,
        Err(err) => {
            eprintln!(
                "error: reading {}/edges.json: {err}. Run `sovereign enrich atlas-resolve \
                 {} --phase all` first.",
                atlas_dir.display(),
                cfg.corpus_id
            );
            return 1;
        }
    };

    // Partition atoms by kind. Configurations coming back may
    // already be in atoms.json from a prior run — we rewrite
    // them wholesale from the new parse rather than appending.
    let mut entities = Vec::new();
    let mut events = Vec::new();
    let mut states = Vec::new();
    let mut relations = Vec::new();
    let mut claims = Vec::new();
    let mut questions = Vec::new();
    let mut argument_reconstructions = Vec::new();
    // Gap-B typed atoms preserved across the configuration rewrite.
    // Phase 8 only adds Configuration atoms; positions + oppositions
    // produced by the Gap-B resolver projection must survive the
    // read-modify-write of atoms.json.
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
            AtomEnvelope::Configuration(_) => {
                // Drop previous Phase 8 output; this pass replaces it.
            }
            AtomEnvelope::ArgumentReconstruction(x) => argument_reconstructions.push(x),
            AtomEnvelope::Position(x) => positions.push(x),
            AtomEnvelope::Opposition(x) => oppositions.push(x),
            AtomEnvelope::Asset(_) => {
                // Configuration-detection pass is prose-shaped; the
                // Asset substrate is preserved through the
                // read-modify-write via the writer (not here).
            }
        }
    }

    // Count distinct section ids across evidence refs for the
    // summary's `section_count` field. This is a cheap proxy for
    // "how long is this corpus" — the LLM uses it to calibrate
    // configuration scope.
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

    let summary = summarise_atlas(
        &entities,
        &events,
        &states,
        &relations,
        &claims,
        &questions,
        &edges_file.edges,
        sections.len(),
        AtlasSummaryParams::default(),
    );

    println!(
        "  loaded atlas: {} entity / {} event / {} state / {} relation / {} claim / {} question atom(s), {} section(s)",
        entities.len(),
        events.len(),
        states.len(),
        relations.len(),
        claims.len(),
        questions.len(),
        sections.len(),
    );

    // Build + dispatch the Phase 8 prompt.
    let Some(prompt): Option<ChatPrompt> = pipeline.compose_phase8_configuration(&summary, &[])
    else {
        eprintln!(
            "error: pipeline `{}` returned None from compose_phase8_configuration despite \
             opting into the phase. This is a pipeline implementation bug.",
            cfg.pipeline_id
        );
        return 1;
    };

    let client = match DaemonInferenceClient::from_enrich_config(&cfg) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("error: building daemon client: {e}");
            return 1;
        }
    };
    let (_embed, chat) = client.into_closures();

    println!("  dispatching Phase 8 configuration prompt…");
    let response = match chat(&prompt).await {
        Ok(r) => r,
        Err(e) => {
            eprintln!("error: Phase 8 chat failed: {e}");
            return 1;
        }
    };

    let items = match pipeline.parse_phase8_configuration(&response) {
        Ok(items) => items,
        Err(e) => {
            eprintln!("error: Phase 8 parse failed: {e}");
            eprintln!(
                "response head:\n{}",
                response.chars().take(800).collect::<String>()
            );
            return 1;
        }
    };

    // Collect known atom ids so parse_configurations can drop any
    // invented references the LLM emits.
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

    // Summary report before writing.
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

    // Write configurations.json first (cheap), then rewrite
    // atoms.json with the configuration atoms merged in.
    let out = ConfigurationsOutput::new(configurations.clone());
    match write_atlas_configurations(&atlas_dir, &out) {
        Ok(path) => println!("  ✓ wrote {}", path.display()),
        Err(e) => {
            eprintln!("error: writing configurations.json: {e}");
            return 1;
        }
    }

    // Read-modify-write atoms.json to merge in the new
    // Configuration atoms. Trajectories stay untouched — this
    // pass didn't regenerate them.
    let trajectories_path = atlas_dir.join("trajectories.json");
    let trajectories: std::collections::BTreeMap<
        String,
        corpus_engine::enrichment::atlas::resolution::Trajectory,
    > = match std::fs::read(&trajectories_path) {
        Ok(bytes) => match serde_json::from_slice::<
            corpus_engine::enrichment::atlas::TrajectoriesFile,
        >(&bytes)
        {
            Ok(file) => serde_json::from_value(file.trajectories).unwrap_or_default(),
            Err(_) => std::collections::BTreeMap::new(),
        },
        Err(_) => std::collections::BTreeMap::new(),
    };

    match write_atlas_full(
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
    ) {
        Ok(_) => {
            println!(
                "  ✓ merged {} configuration(s) into atoms.json",
                configurations.len()
            );
            0
        }
        Err(e) => {
            eprintln!("error: rewriting atoms.json: {e}");
            1
        }
    }
}

fn atlas_dir_for(corpus_id: &str) -> PathBuf {
    paths::index_root(corpus_id).join(ATLAS_DIRNAME)
}

#[derive(Debug)]
struct ParsedConfig {
    corpus_id: String,
}

fn parse_args(args: &[String]) -> Result<ParsedConfig, String> {
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
