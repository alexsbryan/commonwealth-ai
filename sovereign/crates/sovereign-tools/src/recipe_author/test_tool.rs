//! `RecipeTestTool` — drive a sample acquire / extract / chunk
//! against a recipe, with structured per-section miss reporting.
//!
//! Delegates to `corpus_engine`'s [`CorpusEngine::test_recipe`]
//! with `--offline` to skip live HTTP, `embed = false` because no
//! model is loaded inside the tool, and a small `sample_size`
//! tuned for fast LLM iteration loops. Returns a
//! `{validation, extraction, section_misses, passed}` payload —
//! the LLM reads `section_misses` to figure out which regex
//! needs anchoring next.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;

use corpus_engine::{CorpusEngine, EmbedFn, TestOptions};
use sovereign_core::error::{Error, Result};
use sovereign_core::traits::Tool;
use sovereign_core::types::*;

use super::resolve_recipe_path;

#[derive(Default)]
pub struct RecipeTestTool {
    recipes_dir: Option<PathBuf>,
}

impl RecipeTestTool {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_recipes_dir(dir: PathBuf) -> Self {
        Self {
            recipes_dir: Some(dir),
        }
    }
}

#[async_trait]
impl Tool for RecipeTestTool {
    fn descriptor(&self) -> ToolDescriptor {
        ToolDescriptor {
            id: "recipe_test".into(),
            name: "RecipeTest".into(),
            description: "Run the recipe test harness against a recipe TOML. Acquires a \
                 sample of source data, extracts, chunks, and reports per-section \
                 match/miss for html_sections recipes. Pass `params` to inject \
                 install-time parameter values without prompting. Use this AFTER \
                 RecipeValidate passes; iterate on `section_misses[].nearby_text` \
                 to refine regexes."
                .into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description":
                            "Recipe id or relative path under ~/.sovereign/recipes/",
                    },
                    "params": {
                        "type": "object",
                        "description":
                            "Map of parameter name → value (string or array)",
                    },
                    "sample_size": {
                        "type": "integer",
                        "description":
                            "How many source records to sample (default 25)"
                    },
                    "offline": {
                        "type": "boolean",
                        "description":
                            "When true (default), skip HEAD-checks on live URLs"
                    }
                },
                "required": ["path"],
            }),
            examples: vec![ToolExample {
                situation: "Verify a SEC investigation recipe extracts the MD&A and \
                     related-party sections from a sample of 10-Ks before \
                     installing on the full corpus."
                    .into(),
                call: serde_json::json!({
                    "path": "sec-ai-investigation",
                    "params": {
                        "entities": ["NVDA", "MSFT"],
                        "form_types": ["10-K"]
                    },
                    "sample_size": 5
                }),
            }],
            effect: Effect::Read,
            // Sample acquisition + extraction can be slow on
            // multi-MB sources. Mark Slow so the planner doesn't
            // block on it without warning.
            idempotency: Idempotency::Idempotent,
            latency: Latency::Slow,
            scope: Scope::Session,
            output_schema: Some(serde_json::json!({
                "type": "object",
                "properties": {
                    "passed": { "type": "boolean" },
                    "validation": {
                        "type": "object",
                        "properties": {
                            "errors": { "type": "array" },
                            "warnings": { "type": "array" }
                        }
                    },
                    "extraction": {
                        "type": "object",
                        "properties": {
                            "records_attempted": { "type": "integer" },
                            "records_succeeded": { "type": "integer" },
                            "extraction_rate": { "type": "number" }
                        }
                    },
                    "section_misses": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "file": { "type": "string" },
                                "section": { "type": "string" },
                                "nearby_text": { "type": "string" }
                            }
                        }
                    }
                }
            })),
        }
    }

    fn required_permissions(&self) -> Vec<Permission> {
        // Network for the acquire phase + RecipeAuthoring as the
        // umbrella authoring permission. The two together let the
        // approval gate distinguish "I want to author recipes" from
        // "I want generic network access".
        vec![Permission::Network, Permission::RecipeAuthoring]
    }

    async fn execute(&self, params: &serde_json::Value, _ctx: &ToolContext) -> Result<StepOutput> {
        let raw_path = params
            .get("path")
            .and_then(|v| v.as_str())
            .ok_or_else(|| Error::InvalidInput("RecipeTestTool requires `path`".into()))?;
        let resolved = resolve_recipe_path(raw_path, self.recipes_dir.as_ref())?;
        if !resolved.is_file() {
            return Err(Error::InvalidInput(format!(
                "recipe not found at {}",
                resolved.display()
            )));
        }
        let sample_size = params
            .get("sample_size")
            .and_then(|v| v.as_u64())
            .unwrap_or(25) as usize;
        let offline = params
            .get("offline")
            .and_then(|v| v.as_bool())
            .unwrap_or(true);
        let recipe_params = params
            .get("params")
            .and_then(|v| v.as_object())
            .map(|map| {
                let mut out: BTreeMap<String, toml::Value> = BTreeMap::new();
                for (k, v) in map {
                    if let Some(toml_v) = json_to_toml(v) {
                        out.insert(k.clone(), toml_v);
                    }
                }
                out
            })
            .unwrap_or_default();

        let engine = build_stub_engine();
        let options = TestOptions {
            sample_size,
            embed: false,
            offline,
            parameters: recipe_params,
            ..Default::default()
        };
        let report = engine
            .test_recipe(&resolved, &options)
            .await
            .map_err(|e| Error::InvalidInput(format!("{e}")))?;

        let passed = report.passed();
        let extraction = report.extraction.as_ref().map(|e| {
            serde_json::json!({
                "records_attempted": e.records_attempted,
                "records_succeeded": e.records_succeeded,
                "extraction_rate": e.extraction_rate,
            })
        });
        let section_misses = report
            .section_misses
            .iter()
            .map(|m| {
                serde_json::json!({
                    "file": m.file,
                    "section": m.section,
                    "description": m.description,
                    "nearby_text": m.nearby_text,
                })
            })
            .collect::<Vec<_>>();

        // A "passed schema validation but landed zero docs" outcome
        // is the agent's most common silent-failure mode: the recipe
        // is well-formed but points at the wrong host / endpoint /
        // pagination shape, and the agent often retries with another
        // recipe variant instead of confirming the URL works first.
        // Surface a single-line nudge so the agent reaches for
        // probe_url before another draft cycle.
        let nudge = compose_nudge(&report);

        let mut payload = serde_json::json!({
            "passed": passed,
            "path": resolved.display().to_string(),
            "validation": {
                "errors": report.validation.errors,
                "warnings": report.validation.warnings,
            },
            "extraction": extraction,
            "section_misses": section_misses,
        });
        if let Some(n) = nudge {
            payload["nudge"] = serde_json::Value::String(n);
        }
        Ok(StepOutput::Json(payload))
    }
}

/// Compose a single-line "you might want to try X first" nudge
/// based on the test report's failure shape. Returns `None` when
/// the test passed cleanly — we don't want noise on green runs.
fn compose_nudge(report: &corpus_engine::TestReport) -> Option<String> {
    if report.passed() {
        return None;
    }
    // Acquisition / HTTP failures land in `validation.errors` with a
    // characteristic prefix. The fix path is almost always to confirm
    // the URL with `probe_url` (one GET, gets you status / pagination
    // hint / body excerpt) before drafting again.
    let acq_failed = report.validation.errors.iter().any(|e| {
        let l = e.to_ascii_lowercase();
        l.contains("acquisition failed")
            || l.contains("http error")
            || l.contains("dns")
            || l.contains("could not resolve host")
            || l.contains("connection refused")
            || l.contains("certificate")
    });
    if acq_failed {
        return Some(
            "Acquisition failed before any docs were fetched. \
             Don't redraft yet — call `probe_url` against the \
             request URL (with the same headers/params) to confirm \
             the host, version, and auth work. If probe_url errors \
             too, the host is wrong; try a sibling host or ask the \
             partner for the canonical API docs URL."
                .into(),
        );
    }
    // Acquisition succeeded but extraction yielded nothing — usually
    // a wrong `document_path` JSONPath or `content_field` name.
    let zero_extracted = matches!(
        report.extraction.as_ref(),
        Some(e) if e.records_attempted > 0 && e.records_succeeded == 0
    );
    if zero_extracted {
        return Some(
            "Acquisition fetched pages but extraction yielded zero \
             documents. The recipe URL works; the issue is in \
             `[extract]`. Call `probe_url` against the request URL \
             and read the `top_level_keys` / `body_excerpt` to \
             confirm `document_path` resolves to an array of doc \
             objects and `content_field` is a real field name on \
             each."
                .into(),
        );
    }
    None
}

fn build_stub_engine() -> CorpusEngine {
    let stub_embed: EmbedFn = Arc::new(|_text| Box::pin(async { Ok(vec![0f32; 768]) }));
    let tmp = std::env::temp_dir().join("sovereign-recipe-author-test");
    CorpusEngine::new(tmp.clone(), tmp, stub_embed)
}

fn json_to_toml(v: &serde_json::Value) -> Option<toml::Value> {
    match v {
        serde_json::Value::String(s) => Some(toml::Value::String(s.clone())),
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Some(toml::Value::Integer(i))
            } else {
                n.as_f64().map(toml::Value::Float)
            }
        }
        serde_json::Value::Bool(b) => Some(toml::Value::Boolean(*b)),
        serde_json::Value::Array(arr) => Some(toml::Value::Array(
            arr.iter()
                .filter_map(|v| match v {
                    serde_json::Value::String(s) => Some(toml::Value::String(s.clone())),
                    _ => None,
                })
                .collect(),
        )),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> ToolContext {
        ToolContext {
            conversation_id: ConversationId::new(),
            task_id: None,
            working_directory: None,
            in_reasoning_loop: false,
            agent_session_token: None,
            turn_index: 0,
        }
    }

    fn make_root(home: &std::path::Path) -> PathBuf {
        let recipes = home.join(".sovereign/recipes");
        std::fs::create_dir_all(&recipes).unwrap();
        recipes
    }

    #[tokio::test]
    async fn validation_only_run_returns_passed_for_clean_recipe() {
        let home = tempfile::tempdir().unwrap();
        let root = make_root(home.path());
        let dir = root.join("clean");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("recipe.toml"),
            r#"
[corpus]
id = "clean"
name = "clean"
license = "MIT"

[acquire]
type = "bulk_download"
url = "https://example.com/data.zip"

[extract]
type = "plaintext"

[chunk]
type = "sentence"
"#,
        )
        .unwrap();

        // sample_size = 0 = validation-only path through test_recipe.
        let tool = RecipeTestTool::with_recipes_dir(root);
        let out = tool
            .execute(
                &serde_json::json!({"path": "clean", "sample_size": 0, "offline": true}),
                &ctx(),
            )
            .await
            .unwrap();
        match out {
            StepOutput::Json(v) => {
                assert_eq!(v["passed"], true, "got: {v}");
                assert_eq!(v["validation"]["errors"].as_array().unwrap().len(), 0);
            }
            other => panic!("expected Json, got {other:?}"),
        }
    }

    #[test]
    fn json_to_toml_handles_arrays_of_strings() {
        let v = serde_json::json!(["NVDA", "MSFT"]);
        let toml_v = json_to_toml(&v).unwrap();
        match toml_v {
            toml::Value::Array(a) => assert_eq!(a.len(), 2),
            other => panic!("expected Array, got {other:?}"),
        }
    }
}
