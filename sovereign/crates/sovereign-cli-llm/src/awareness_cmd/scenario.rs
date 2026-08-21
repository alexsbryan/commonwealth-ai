// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn awareness scenario` — drive a scripted end-to-end run.
//!
//! Parses a TOML scenario file (see `tests/awareness/scenarios/`),
//! seeds the sandbox StateStore from the named template, runs
//! extraction with the resolved InferenceFn, then evaluates each
//! `[[assertions]]` block against the resulting state. Reports
//! per-assertion pass/fail.
//!
//! Phase 4 ships the four most useful assertion shapes:
//!
//!   - `entity_count` — counts of person/org/initiative atoms
//!   - `entity_present` — a specific named entity must be extracted
//!   - `digest_contains` — the rendered digest block contains a
//!     substring (typically an entity name)
//!   - `suggestion_quality` — runs `suggest` per conversation, then
//!     scores against the template's `expected_suggestions`
//!
//! Spec-listed shapes deferred (require additional plumbing):
//! `query_synthesis` (needs runtime invocation), `atos_composition`
//! (needs project.toml shipped with the scenario), `decay_differential`
//! (needs synthetic Memory rows in the seed).

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use corpus_engine::enrichment::atlas::atoms::AtomEnvelope;
use corpus_engine::enrichment::atlas::writer::read_atlas_atoms;
use corpus_engine::enrichment::pipeline::atlas::EntityType;
use corpus_engine::InferenceFn;
use serde::Deserialize;
use sovereign_core::traits::ConversationStore;
use sovereign_store::sqlite::SqliteStateStore;
use sovereign_tools::knowledge_view::splice_extension::{
    load_chunk_timestamps, AtosSnapshot, ConversationCorpus,
};
use sovereign_tools::knowledge_view::timeline::{
    assemble_timelines_from_atlas, InteractionTimeline,
};

use super::args::parse_args;
use super::golden::{
    score_entities, score_suggestions, DetectedSuggestion, EntityScore, GoldenSet,
};
use super::render::display_path;
use super::store_open::atlas_dir_for;
use super::templates::{load_builtin, load_from_path, Template};

const RELATIONAL_VIEWS: &[&str] = &["personal-knowledge", "conversation-history"];

pub(super) async fn cmd_scenario(args: &[String]) -> i32 {
    let flags = match parse_args(args) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("awareness: {e}");
            return 2;
        }
    };
    let positional = flags.positionals();
    let Some(path) = positional.into_iter().next() else {
        eprintln!("awareness scenario: <path-to-toml> is required");
        return 2;
    };
    let _interactive = flags.has("interactive");
    let output_dir = flags
        .value("output")
        .map(|s| s.to_string())
        .filter(|s| !s.is_empty())
        .map(PathBuf::from);

    let body = match std::fs::read_to_string(&path) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("awareness scenario: read {path}: {e}");
            return 1;
        }
    };
    let script: ScenarioScript = match toml::from_str(&body) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("awareness scenario: parse {path}: {e}");
            return 2;
        }
    };

    println!("═══ Scenario: {} ═══", script.scenario.name);
    if !script.scenario.description.is_empty() {
        println!("{}", script.scenario.description);
    }
    println!();

    // Resolve the template.
    let template = match resolve_template(&script.scenario) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("awareness scenario: {e}");
            return 1;
        }
    };
    let golden = GoldenSet::from_template(&template);

    // Resolve sandbox.
    let sandbox = script
        .scenario
        .sandbox
        .clone()
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            let mut p = std::env::temp_dir();
            p.push(format!("awareness-scenario-{}", script.scenario.name));
            p
        });
    println!("Sandbox: {}", display_path(&sandbox));

    // Reset.
    let runner = script.runner.clone().unwrap_or_default();
    if runner.reset_before {
        if sandbox.exists() {
            if let Err(e) = std::fs::remove_dir_all(&sandbox) {
                eprintln!(
                    "awareness scenario: reset {} failed: {e}",
                    display_path(&sandbox)
                );
                return 1;
            }
        }
    }
    if let Err(e) = std::fs::create_dir_all(&sandbox) {
        eprintln!(
            "awareness scenario: create sandbox {}: {e}",
            display_path(&sandbox)
        );
        return 1;
    }

    // Default to real (talk to the daemon) — mock reads prompt
    // examples and gives misleading "good" signal that's not useful
    // for tuning. Scenario authors can opt into mock for offline
    // wiring checks.
    let inference_mode = runner.inference.as_deref().unwrap_or("real");

    // ── Step 1: Seed ─────────────────────────────────────────────
    print_step("Seed", &template, &sandbox);
    let seed_args = build_seed_args(&script.scenario, &sandbox);
    let code = super::seed::cmd_seed(&seed_args).await;
    if code != 0 {
        eprintln!("awareness scenario: seed step failed (exit {code})");
        return 1;
    }

    // ── Step 2: Extract ──────────────────────────────────────────
    print_step("Extract", &template, &sandbox);
    let mut extract_args = vec!["extract".to_string()];
    // "real" is the default in extract::cmd_extract; only inject the
    // flag for the explicit alternatives. The flag splitter treats
    // unknown bare flags as value-flags and would consume the next
    // token (e.g. "--db-path") if we passed `--real`.
    match inference_mode {
        "mock" => extract_args.push("--mock".to_string()),
        "dry_run" | "dry-run" => extract_args.push("--dry-run".to_string()),
        _ => {} // real (default), or anything else — fall through.
    }
    extract_args.push("--db-path".to_string());
    extract_args.push(sandbox.display().to_string());
    let code = super::extract::cmd_extract(&extract_args[1..]).await;
    if code != 0 {
        eprintln!("awareness scenario: extract step failed (exit {code})");
        return 1;
    }

    // ── Step 3: Drive suggest replay (only if any assertion needs it) ──
    let needs_suggestions = script
        .assertions
        .iter()
        .any(|a| matches!(a, Assertion::SuggestionQuality { .. }));
    let detected_suggestions = if needs_suggestions {
        match drive_suggest_replay(&sandbox, &template, inference_mode).await {
            Ok(v) => v,
            Err(e) => {
                eprintln!("awareness scenario: suggest replay failed: {e}");
                return 1;
            }
        }
    } else {
        Vec::new()
    };

    // ── Step 4: Run assertions ───────────────────────────────────
    println!();
    println!("─── Assertions ───");
    let mut passed = 0usize;
    let mut failed = 0usize;
    let mut report_blocks: Vec<serde_json::Value> = Vec::new();
    for (idx, assertion) in script.assertions.iter().enumerate() {
        let result = run_assertion(&sandbox, assertion, &golden, &detected_suggestions);
        match &result.outcome {
            Outcome::Pass => passed += 1,
            Outcome::Fail => failed += 1,
        }
        print_assertion_result(idx + 1, assertion, &result);
        report_blocks.push(result.to_json(idx + 1, assertion));
    }

    println!();
    println!("Result: {}/{} assertions passed", passed, passed + failed);

    // Output dir.
    if let Some(dir) = output_dir {
        let _ = std::fs::create_dir_all(&dir);
        let report = serde_json::json!({
            "scenario": script.scenario.name,
            "passed": passed,
            "failed": failed,
            "assertions": report_blocks,
        });
        let report_path = dir.join("scenario-report.json");
        if let Err(e) = std::fs::write(
            &report_path,
            serde_json::to_string_pretty(&report).unwrap_or_default(),
        ) {
            eprintln!(
                "awareness scenario: failed to write {} : {e}",
                display_path(&report_path)
            );
        } else {
            println!("Wrote {}", display_path(&report_path));
        }
        copy_atlas_artifacts(&sandbox, &dir);
    }

    if failed == 0 {
        0
    } else {
        1
    }
}

fn print_step(name: &str, template: &Template, sandbox: &Path) {
    println!(
        "─── {name} (template '{}', sandbox {}) ───",
        template.meta.name,
        display_path(sandbox)
    );
}

fn build_seed_args(scenario: &Scenario, sandbox: &Path) -> Vec<String> {
    let mut out = Vec::new();
    if let Some(name) = &scenario.template {
        out.push("--from-template".into());
        out.push(name.clone());
    } else if let Some(p) = &scenario.from_file {
        out.push("--from-file".into());
        out.push(p.clone());
    }
    out.push("--db-path".into());
    out.push(sandbox.display().to_string());
    out
}

fn resolve_template(scenario: &Scenario) -> Result<Template, String> {
    if let Some(name) = &scenario.template {
        return load_builtin(name);
    }
    if let Some(p) = &scenario.from_file {
        return load_from_path(Path::new(p));
    }
    Err("scenario must specify either `template = \"<name>\"` or `from_file = \"<path>\"`".into())
}

#[derive(Debug, Deserialize)]
struct ScenarioScript {
    scenario: Scenario,
    #[serde(default)]
    runner: Option<RunnerConfig>,
    #[serde(default)]
    assertions: Vec<Assertion>,
}

/// An awareness-scenario manifest — unrelated to
/// `sovereign_mesh::mesh_sim::scenario::Scenario` (a simulated mesh
/// topology) or the voice/moral bench scenarios.
#[derive(Debug, Deserialize)]
struct Scenario {
    name: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    template: Option<String>,
    #[serde(default)]
    from_file: Option<String>,
    #[serde(default)]
    sandbox: Option<String>,
}

/// NOT `corpus_engine::sovereign_config::RunnerConfig`, which configures a
/// watcher SUBPROCESS (command, cwd, timeout, debounce); this is the two
/// knobs a scenario run takes.
#[derive(Debug, Deserialize, Clone)]
struct RunnerConfig {
    /// "mock" | "dry_run" | "real". Default "mock".
    #[serde(default)]
    inference: Option<String>,
    /// rm -rf the sandbox before seeding. Default true.
    #[serde(default = "default_reset_before")]
    reset_before: bool,
}

impl Default for RunnerConfig {
    fn default() -> Self {
        Self {
            inference: None,
            reset_before: default_reset_before(),
        }
    }
}

fn default_reset_before() -> bool {
    true
}

#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum Assertion {
    EntityCount {
        #[serde(default)]
        min_people: Option<usize>,
        #[serde(default)]
        min_organizations: Option<usize>,
        #[serde(default)]
        min_initiatives: Option<usize>,
    },
    EntityPresent {
        name: String,
    },
    DigestContains {
        digest: String, // "relational" | "strategic"
        substring: String,
    },
    SuggestionQuality {
        #[serde(default)]
        f1_min: Option<f64>,
        #[serde(default)]
        precision_min: Option<f64>,
        #[serde(default)]
        recall_min: Option<f64>,
    },
    EntityF1 {
        f1_min: f64,
    },
}

#[derive(Debug)]
struct AssertionResult {
    outcome: Outcome,
    detail: String,
}

/// A local {Pass, Fail} for one assertion — not
/// `sovereign_pipeline::adaptive::Outcome`, nor the `cognitive::scorer`
/// `Outcome` in the `sovereign-eval` crate, which score work, not assertions.
///
/// That second crate is named in prose, hyphenated as Cargo spells it, rather
/// than as a Rust path: `backstage_boundary` in `lib.rs` fails the build if
/// any module outside `bench_cmd` spells the underscore form, and a doc
/// comment counts. The rule is containment of the back-of-house instrument
/// (`quality/ARCH_LAYERS.toml`), and it is textual by design.
#[derive(Debug, PartialEq, Eq)]
enum Outcome {
    Pass,
    Fail,
}

impl AssertionResult {
    fn pass(detail: impl Into<String>) -> Self {
        Self {
            outcome: Outcome::Pass,
            detail: detail.into(),
        }
    }
    fn fail(detail: impl Into<String>) -> Self {
        Self {
            outcome: Outcome::Fail,
            detail: detail.into(),
        }
    }
    fn to_json(&self, index: usize, assertion: &Assertion) -> serde_json::Value {
        serde_json::json!({
            "index": index,
            "kind": assertion_kind(assertion),
            "outcome": match self.outcome {
                Outcome::Pass => "pass",
                Outcome::Fail => "fail",
            },
            "detail": self.detail,
        })
    }
}

fn assertion_kind(a: &Assertion) -> &'static str {
    match a {
        Assertion::EntityCount { .. } => "entity_count",
        Assertion::EntityPresent { .. } => "entity_present",
        Assertion::DigestContains { .. } => "digest_contains",
        Assertion::SuggestionQuality { .. } => "suggestion_quality",
        Assertion::EntityF1 { .. } => "entity_f1",
    }
}

fn print_assertion_result(index: usize, a: &Assertion, r: &AssertionResult) {
    let mark = match r.outcome {
        Outcome::Pass => "✓",
        Outcome::Fail => "✗",
    };
    println!("  {} #{} {}: {}", mark, index, assertion_kind(a), r.detail);
}

fn run_assertion(
    sandbox: &Path,
    a: &Assertion,
    golden: &GoldenSet,
    detected: &[DetectedSuggestion],
) -> AssertionResult {
    let counts = count_entities(sandbox);
    match a {
        Assertion::EntityCount {
            min_people,
            min_organizations,
            min_initiatives,
        } => {
            let mut violations: Vec<String> = Vec::new();
            if let Some(n) = min_people {
                if counts.people < *n {
                    violations.push(format!("people {} < {n}", counts.people));
                }
            }
            if let Some(n) = min_organizations {
                if counts.organizations < *n {
                    violations.push(format!("organizations {} < {n}", counts.organizations));
                }
            }
            if let Some(n) = min_initiatives {
                if counts.initiatives < *n {
                    violations.push(format!("initiatives {} < {n}", counts.initiatives));
                }
            }
            if violations.is_empty() {
                AssertionResult::pass(format!(
                    "{} people, {} organizations, {} initiatives",
                    counts.people, counts.organizations, counts.initiatives
                ))
            } else {
                AssertionResult::fail(violations.join("; "))
            }
        }
        Assertion::EntityPresent { name } => {
            let folded = name.trim().to_lowercase();
            if counts
                .names
                .iter()
                .any(|n| n.trim().to_lowercase() == folded)
            {
                AssertionResult::pass(format!("\"{name}\" extracted"))
            } else {
                AssertionResult::fail(format!("\"{name}\" not in extracted set"))
            }
        }
        Assertion::DigestContains { digest, substring } => {
            let block = render_digest_block(sandbox, digest);
            if block.to_lowercase().contains(&substring.to_lowercase()) {
                AssertionResult::pass(format!("\"{substring}\" present in {digest} block"))
            } else {
                AssertionResult::fail(format!(
                    "\"{substring}\" not in {digest} block (block was: {})",
                    if block.is_empty() {
                        "empty"
                    } else {
                        "present but mismatched"
                    }
                ))
            }
        }
        Assertion::SuggestionQuality {
            f1_min,
            precision_min,
            recall_min,
        } => {
            let s = score_suggestions(&golden.expected_suggestions, detected);
            let mut violations: Vec<String> = Vec::new();
            if let Some(t) = f1_min {
                if s.f1() < *t {
                    violations.push(format!("F1 {:.2} < {:.2}", s.f1(), t));
                }
            }
            if let Some(t) = precision_min {
                if s.precision() < *t {
                    violations.push(format!("P {:.2} < {:.2}", s.precision(), t));
                }
            }
            if let Some(t) = recall_min {
                if s.recall() < *t {
                    violations.push(format!("R {:.2} < {:.2}", s.recall(), t));
                }
            }
            let summary = format!(
                "F1 {:.2}, P {:.2}, R {:.2} ({}/{} matched, {} fired, {} missed)",
                s.f1(),
                s.precision(),
                s.recall(),
                s.matched,
                s.expected,
                s.fired,
                s.missed.len()
            );
            if violations.is_empty() {
                AssertionResult::pass(summary)
            } else {
                AssertionResult::fail(format!("{} ({})", violations.join("; "), summary))
            }
        }
        Assertion::EntityF1 { f1_min } => {
            let score = entity_score(sandbox, &golden);
            if score.f1() >= *f1_min {
                AssertionResult::pass(format!(
                    "F1 {:.2} ≥ {:.2} ({}/{} matched)",
                    score.f1(),
                    f1_min,
                    score.matched,
                    score.expected
                ))
            } else {
                AssertionResult::fail(format!(
                    "F1 {:.2} < {:.2} ({}/{} matched, {} false positives)",
                    score.f1(),
                    f1_min,
                    score.matched,
                    score.expected,
                    score.false_positives.len()
                ))
            }
        }
    }
}

#[derive(Debug, Default)]
struct EntityCounts {
    people: usize,
    organizations: usize,
    initiatives: usize,
    names: Vec<String>,
}

fn count_entities(sandbox: &Path) -> EntityCounts {
    let mut counts = EntityCounts::default();
    for view_id in RELATIONAL_VIEWS {
        let dir = atlas_dir_for(sandbox, view_id);
        if !dir.exists() {
            continue;
        }
        let Ok(file) = read_atlas_atoms(&dir) else {
            continue;
        };
        for atom in file.atoms {
            if let AtomEnvelope::Entity(e) = atom {
                match e.entity_type {
                    EntityType::Person => counts.people += 1,
                    EntityType::Institution => counts.organizations += 1,
                    EntityType::Initiative => counts.initiatives += 1,
                    _ => continue,
                }
                counts.names.push(e.canonical_name);
            }
        }
    }
    counts
}

fn entity_score(sandbox: &Path, golden: &GoldenSet) -> EntityScore {
    let counts = count_entities(sandbox);
    score_entities(&golden.expected_entities, &counts.names)
}

/// Render the named digest block as it would appear on the next
/// turn (no current-turn context). Reuses the same pure formatters
/// the production splice path uses; mirrors the `digest` subcommand
/// minus the printing.
fn render_digest_block(sandbox: &Path, which: &str) -> String {
    let chunk_ts = load_chunk_timestamps(&sandbox.join("state.db"));
    let resolver = move |id: &str| -> Option<i64> { chunk_ts.get(id).copied() };
    let atos = AtosSnapshot::empty(); // scenarios run sandboxed; no project.toml in scope
    let mut all_timelines: Vec<InteractionTimeline> = Vec::new();
    for view_id in RELATIONAL_VIEWS {
        let corpus_dir = sandbox.join("indexes").join(view_id);
        if !atlas_dir_for(sandbox, view_id).exists() {
            continue;
        }
        if let Ok(mut t) = assemble_timelines_from_atlas(&corpus_dir, &resolver, &atos) {
            all_timelines.append(&mut t);
        }
    }
    let now = unix_now();
    let corpus = ConversationCorpus::from_messages(Vec::<String>::new());
    let in_conv = move |entity: &str| -> bool { corpus.contains_entity(entity) };
    use sovereign_tools::knowledge_view::relational::format_relational;
    use sovereign_tools::knowledge_view::strategic::format_strategic;
    use sovereign_tools::knowledge_view::view_kind::ViewKind;
    let no_rel_notes = |_: &str| Vec::new();
    let no_strat_goals = |_: &str| Vec::new();

    match which {
        "relational" => {
            format_relational(
                &all_timelines,
                &no_rel_notes,
                &in_conv,
                now,
                ViewKind::Relational.default_budget_tokens(),
            )
            .0
        }
        "strategic" => {
            format_strategic(
                &all_timelines,
                &no_strat_goals,
                &in_conv,
                now,
                ViewKind::Strategic.default_budget_tokens(),
            )
            .0
        }
        _ => String::new(),
    }
}

use sovereign_core::time::unix_now;

/// Replay `awareness suggest` per conversation and collect detected
/// suggestions into the shape `golden::score_suggestions` expects.
async fn drive_suggest_replay(
    sandbox: &Path,
    template: &Template,
    inference_mode: &str,
) -> Result<Vec<DetectedSuggestion>, String> {
    let store = SqliteStateStore::open(&sandbox.join("state.db"))
        .map_err(|e| format!("open sandbox state.db: {e}"))?;
    let store = Arc::new(store);

    // Resolve inference once, then reuse for every conversation.
    // Synthesised through the SAME parser the CLI uses, rather than a
    // hand-built pair list: one decider for what a flag means, whoever
    // is asking.
    let mut argv: Vec<String> = vec!["--db-path".into(), sandbox.display().to_string()];
    match inference_mode {
        "mock" => argv.push("--mock".into()),
        "dry_run" => argv.push("--dry-run".into()),
        _ => {} // "real" → no flag
    }
    let flag_kv =
        super::args::parse_args(&argv).map_err(|e| format!("scenario inference flags: {e}"))?;

    let (inference, _mode) = super::inference::resolve_inference(&flag_kv)
        .await
        .map_err(|e| format!("inference: {e}"))?;

    let mut detected: Vec<DetectedSuggestion> = Vec::new();
    for c in &template.conversations {
        let conversation = match store.get_conversation(&c.id).await {
            Ok(c) => c,
            Err(_) => continue, // not seeded; skip
        };
        for (idx, msg) in conversation.messages.iter().enumerate() {
            if msg.role != sovereign_core::types::Role::User {
                continue;
            }
            let prompt = build_detection_prompt(&conversation, idx);
            let raw = match (inference)(&prompt, None).await {
                Ok(r) => r,
                Err(_) => continue,
            };
            for d in parse_suggest_detections(&raw, &conversation.id, (idx + 1) as u32) {
                detected.push(d);
            }
        }
    }
    Ok(detected)
}

/// Mirrors `suggest::build_detection_prompt`. Lives here too rather
/// than crossing module boundaries; same shape so the mock matches
/// either entry point.
fn build_detection_prompt(c: &sovereign_core::types::Conversation, turn_idx: usize) -> String {
    let mut window = String::new();
    let start = turn_idx.saturating_sub(3);
    for (i, m) in c
        .messages
        .iter()
        .enumerate()
        .skip(start)
        .take(turn_idx + 1 - start)
    {
        let role = match m.role {
            sovereign_core::types::Role::User => "user",
            sovereign_core::types::Role::Assistant => "assistant",
            sovereign_core::types::Role::System => "system",
        };
        let marker = if i == turn_idx {
            " ← current turn"
        } else {
            ""
        };
        window.push_str(&format!(
            "[Turn {}, {}{}]\n{}\n\n",
            i + 1,
            role,
            marker,
            m.content
        ));
    }
    format!(
        r#"You are a development-time evaluator for the Sovereign suggest_note pipeline.

Read the conversation excerpt below. For the CURRENT TURN only, decide:

  - **commitment**: did the user explicitly commit to an action with an external party, deadline, or deliverable?
  - **follow_up**: did the user say they will revisit something later?
  - **goal**: did the user state a measurable objective?

Conversation excerpt:

{window}

Respond ONLY with JSON: {{"detections":[{{"kind":"commitment|follow_up|goal","content":"…","related_entity":"…|null","reasoning":"…"}}]}}"#
    )
}

fn parse_suggest_detections(
    raw: &str,
    conversation_id: &str,
    turn: u32,
) -> Vec<DetectedSuggestion> {
    let trimmed = trim_to_object(raw);
    let value: serde_json::Value = match serde_json::from_str(trimmed) {
        Ok(v) => v,
        Err(_) => return Vec::new(),
    };
    let arr = match value.get("detections").and_then(|v| v.as_array()) {
        Some(a) => a,
        None => return Vec::new(),
    };
    arr.iter()
        .filter_map(|v| {
            let kind = v.get("kind")?.as_str()?.to_string();
            let content = v.get("content")?.as_str()?.trim().to_string();
            if content.is_empty() {
                return None;
            }
            let related_entity = v
                .get("related_entity")
                .and_then(|x| x.as_str())
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty() && s != "null");
            Some(DetectedSuggestion {
                conversation_id: conversation_id.into(),
                turn,
                kind,
                content,
                related_entity,
            })
        })
        .collect()
}

fn trim_to_object(s: &str) -> &str {
    let start = match s.find('{') {
        Some(i) => i,
        None => return s,
    };
    let end = match s.rfind('}') {
        Some(i) => i,
        None => return s,
    };
    if end > start {
        &s[start..=end]
    } else {
        s
    }
}

fn copy_atlas_artifacts(sandbox: &Path, dest: &Path) {
    for view_id in RELATIONAL_VIEWS {
        let dir = atlas_dir_for(sandbox, view_id);
        for f in ["atoms.json", "edges.json"] {
            let from = dir.join(f);
            if from.exists() {
                let to = dest.join(format!("{view_id}-{f}"));
                let _ = std::fs::copy(&from, &to);
            }
        }
    }
}

// Suppress unused import on `Command` — it's reserved for the
// scenario `--interactive` REPL spawn path that lands later.
#[allow(dead_code)]
fn _force_use(_c: Command) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_minimal_scenario_script() {
        let body = r#"
[scenario]
name = "tiny"
template = "consulting"

[[assertions]]
kind = "entity_count"
min_people = 1
"#;
        let s: ScenarioScript = toml::from_str(body).unwrap();
        assert_eq!(s.scenario.name, "tiny");
        assert_eq!(s.assertions.len(), 1);
        match &s.assertions[0] {
            Assertion::EntityCount { min_people, .. } => assert_eq!(*min_people, Some(1)),
            _ => panic!("wrong assertion kind"),
        }
    }

    #[test]
    fn parse_all_assertion_kinds() {
        let body = r#"
[scenario]
name = "all"
template = "consulting"

[[assertions]]
kind = "entity_count"
min_people = 3

[[assertions]]
kind = "entity_present"
name = "Sarah Chen"

[[assertions]]
kind = "digest_contains"
digest = "relational"
substring = "Sarah"

[[assertions]]
kind = "suggestion_quality"
f1_min = 0.5

[[assertions]]
kind = "entity_f1"
f1_min = 0.6
"#;
        let s: ScenarioScript = toml::from_str(body).unwrap();
        assert_eq!(s.assertions.len(), 5);
    }

    #[test]
    fn assertion_kind_label_matches_each_variant() {
        assert_eq!(
            assertion_kind(&Assertion::EntityCount {
                min_people: None,
                min_organizations: None,
                min_initiatives: None
            }),
            "entity_count"
        );
        assert_eq!(
            assertion_kind(&Assertion::EntityPresent { name: "x".into() }),
            "entity_present"
        );
        assert_eq!(
            assertion_kind(&Assertion::DigestContains {
                digest: "relational".into(),
                substring: "x".into()
            }),
            "digest_contains"
        );
        assert_eq!(
            assertion_kind(&Assertion::SuggestionQuality {
                f1_min: None,
                precision_min: None,
                recall_min: None
            }),
            "suggestion_quality"
        );
        assert_eq!(
            assertion_kind(&Assertion::EntityF1 { f1_min: 0.5 }),
            "entity_f1"
        );
    }
}
