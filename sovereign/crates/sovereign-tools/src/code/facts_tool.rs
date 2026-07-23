// SPDX-License-Identifier: AGPL-3.0-or-later
//! `facts` — expose the deterministic, tree-sitter code fact base to agents.
//!
//! Sibling to the SCIP call-graph tools (`symbols`/`callers`/`callees`): those
//! answer "how do symbols connect?"; `facts` answers "what does the code
//! literally state?" — every function definition, every `Type { field: value }`
//! construction-site (the data-flow fact behind config claims), and every
//! string literal, each cited to an exact `file:line`. It is the same fact base
//! `sovereign code facts` builds and `check-spec` audits against, surfaced so an
//! agent can ask it directly instead of shelling out or reading source.
//!
//! ## Why this is safe to expose (no contention)
//!
//! The fact base is a pure tree-sitter read — NO embeddings, NO rust-analyzer,
//! NO model. Reading it never touches the inference slots, so unlike the
//! semantic (`code_search`) plane it cannot contend with agents mid-turn. This
//! is the embed-free "structural" plane (see docs/CHECK_CODE_AGAINST_SPEC.md).
//!
//! ## Dependability: every answer is freshness-stamped
//!
//! The fact base is built by an explicit `sovereign code facts` run (and, once
//! the structural watcher is wired, kept fresh incrementally). It can therefore
//! LAG the code. The one failure mode that would make this tool untrustworthy
//! is serving stale facts as if they were current — so every response carries a
//! `freshness` block: when the facts were built, how old that is, and whether
//! the code graph (`scip_graph.db`) has moved since. An agent always knows how
//! much to trust what it just read. Honest staleness over silent staleness.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::json;
use tokio::sync::Mutex;

use corpus_engine::facts::Facts;
use sovereign_core::error::{Error, Result};
use sovereign_core::traits::Tool;
use sovereign_core::types::*;

/// One parsed fact base plus the mtime it was parsed at, so repeated queries
/// against an unchanged `facts.json` don't re-parse a large file (the corpus
/// fact base can be tens of MB — re-parsing it per agent query is exactly the
/// raw-token/CPU waste the code-intel path exists to avoid).
struct CachedFacts {
    mtime_secs: i64,
    facts: Arc<Facts>,
}

pub struct FactsTool {
    /// Root under which each corpus has `<corpus_id>/facts.json`.
    indexes_dir: PathBuf,
    /// mtime-keyed parse cache, shared across concurrent calls.
    cache: Arc<Mutex<HashMap<PathBuf, CachedFacts>>>,
}

impl FactsTool {
    pub fn new(indexes_dir: impl Into<PathBuf>) -> Self {
        Self {
            indexes_dir: indexes_dir.into(),
            cache: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Load `facts.json`, using the mtime-keyed cache when the file is
    /// unchanged since the last parse. Returns `None` when the file is absent.
    async fn load_facts(&self, facts_path: &Path) -> Option<Arc<Facts>> {
        let mtime = mtime_secs(facts_path)?;
        {
            let cache = self.cache.lock().await;
            if let Some(c) = cache.get(facts_path) {
                if c.mtime_secs == mtime {
                    return Some(Arc::clone(&c.facts));
                }
            }
        }
        // Cache miss / stale — parse and store. `Facts::load` is a full read;
        // do it outside the lock so concurrent queries on OTHER corpora aren't
        // blocked behind a large parse.
        let facts = Arc::new(Facts::load(facts_path).ok()?);
        let mut cache = self.cache.lock().await;
        cache.insert(
            facts_path.to_path_buf(),
            CachedFacts {
                mtime_secs: mtime,
                facts: Arc::clone(&facts),
            },
        );
        Some(facts)
    }

    /// Corpora to search: the named one, or every corpus dir that has a
    /// `facts.json` when none is named.
    fn resolve_corpora(&self, corpus_id: Option<&str>) -> Vec<String> {
        if let Some(id) = corpus_id {
            return vec![id.to_string()];
        }
        let mut out = Vec::new();
        if let Ok(entries) = std::fs::read_dir(&self.indexes_dir) {
            for e in entries.flatten() {
                if e.path().join("facts.json").is_file() {
                    if let Some(name) = e.file_name().to_str() {
                        out.push(name.to_string());
                    }
                }
            }
        }
        out.sort();
        out
    }
}

#[async_trait]
impl Tool for FactsTool {
    fn descriptor(&self) -> ToolDescriptor {
        ToolDescriptor {
            id: "facts".to_string(),
            name: "Code Facts".to_string(),
            description:
                "Query the deterministic, tree-sitter code fact base: function \
                 definitions, `Type { field: value }` construction-site config values, \
                 and string literals — each cited to an exact file:line. The \
                 embed-free structural companion to `symbols`/`callers` (call graph) \
                 and `code_search` (semantic). \
                 \
                 Use it to answer, WITHOUT reading source: does function X exist and \
                 where? What value does config field Y get constructed with? Where is \
                 literal Z written? It reads the same fact base `sovereign code facts` \
                 builds and `check-spec` audits. \
                 \
                 Reading facts NEVER touches the inference/embedding slots, so it is \
                 safe to call freely mid-task. Every response is freshness-stamped \
                 (`freshness`): when the facts were built, their age, and whether the \
                 code graph has moved since — so you always know how much to trust \
                 them. If `status` is `no_facts`, the fact base has not been built for \
                 that corpus yet (`sovereign code facts <repo> --corpus-id <id>`)."
                    .to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "Name or substring to match. Matched (case-insensitive) against function names, construction struct/field/value, and literal content. Required."
                    },
                    "corpus_id": {
                        "type": "string",
                        "description": "Which corpus's facts to query. Omit to search every corpus that has a built fact base."
                    },
                    "kind": {
                        "type": "string",
                        "enum": ["function", "config", "literal", "all"],
                        "description": "Restrict to one fact kind. Default `all`. `function` = definitions, `config` = construction-site field values, `literal` = string literals."
                    },
                    "limit": {
                        "type": "integer",
                        "description": "Max results PER fact kind (default 30)."
                    }
                },
                "required": ["query"]
            }),
            examples: vec![
                ToolExample {
                    situation: "Confirm `export_changed` exists and find where it's defined, without opening the file.".into(),
                    call: json!({ "query": "export_changed", "kind": "function" }),
                },
                ToolExample {
                    situation: "Find what value the `debounce` field is constructed with across the codebase (a config fact).".into(),
                    call: json!({ "query": "debounce", "kind": "config" }),
                },
                ToolExample {
                    situation: "Locate every place the string `never_run` is written, scoped to one corpus.".into(),
                    call: json!({ "query": "never_run", "kind": "literal", "corpus_id": "commonwealth-ai" }),
                },
            ],
            effect: Effect::Read,
            idempotency: Idempotency::Idempotent,
            latency: Latency::Fast,
            scope: Scope::Session,
            output_schema: Some(json!({
                "type": "object",
                "properties": {
                    "status": {
                        "type": "string",
                        "enum": ["ok", "no_facts", "no_matches"],
                        "description": "`ok` = matches found. `no_facts` = no fact base built for the searched corpora. `no_matches` = fact base present but query matched nothing."
                    },
                    "query": { "type": "string" },
                    "corpora_searched": { "type": "array", "items": { "type": "string" } },
                    "match_count": { "type": "integer" },
                    "freshness": {
                        "type": "object",
                        "description": "Per the OLDEST fact base searched — the worst case. `lags_graph` true means the SCIP graph was rebuilt after these facts were extracted, so a recent code change may not be reflected here.",
                        "properties": {
                            "built_at_unix": { "type": ["integer", "null"] },
                            "age_hours": { "type": ["number", "null"] },
                            "staleness": { "type": "string", "enum": ["fresh", "aging", "stale", "unknown"] },
                            "lags_graph": { "type": "boolean" },
                            "note": { "type": "string" }
                        }
                    },
                    "functions": {
                        "type": "array",
                        "items": { "type": "object", "properties": {
                            "name": { "type": "string" }, "file": { "type": "string" },
                            "line": { "type": "integer" }, "corpus": { "type": "string" }
                        }}
                    },
                    "config": {
                        "type": "array",
                        "items": { "type": "object", "properties": {
                            "struct_type": { "type": "string" }, "field": { "type": "string" },
                            "value": { "type": "string" }, "enclosing_fn": { "type": "string" },
                            "file": { "type": "string" }, "line": { "type": "integer" },
                            "corpus": { "type": "string" }
                        }}
                    },
                    "literals": {
                        "type": "array",
                        "items": { "type": "object", "properties": {
                            "content": { "type": "string" }, "enclosing_fn": { "type": "string" },
                            "file": { "type": "string" }, "line": { "type": "integer" },
                            "corpus": { "type": "string" }
                        }}
                    }
                }
            })),
        }
    }

    fn required_permissions(&self) -> Vec<Permission> {
        vec![]
    }

    async fn execute(&self, params: &serde_json::Value, _ctx: &ToolContext) -> Result<StepOutput> {
        let query = params
            .get("query")
            .and_then(|v| v.as_str())
            .ok_or_else(|| Error::InvalidInput("`facts` requires a `query`".into()))?
            .trim();
        if query.is_empty() {
            return Err(Error::InvalidInput("`query` must not be empty".into()));
        }
        let lc = query.to_lowercase();
        let corpus_id = params.get("corpus_id").and_then(|v| v.as_str());
        let kind = params.get("kind").and_then(|v| v.as_str()).unwrap_or("all");
        let want = |k: &str| kind == "all" || kind == k;
        let limit = params
            .get("limit")
            .and_then(|v| v.as_u64())
            .map(|n| n as usize)
            .unwrap_or(30);

        let corpora = self.resolve_corpora(corpus_id);

        let mut functions = Vec::new();
        let mut config = Vec::new();
        let mut literals = Vec::new();
        // Track the OLDEST fact base actually read, for a worst-case freshness
        // stamp, plus whether any searched corpus's graph is newer than its facts.
        let mut oldest_built: Option<i64> = None;
        let mut any_lags_graph = false;
        let mut any_facts_present = false;

        for corpus in &corpora {
            let corpus_dir = self.indexes_dir.join(corpus);
            let facts_path = corpus_dir.join("facts.json");
            let facts = match self.load_facts(&facts_path).await {
                Some(f) => f,
                None => continue, // no fact base for this corpus — skip, reported via status
            };
            any_facts_present = true;

            // Freshness inputs for this corpus.
            if let Some(built) = mtime_secs(&facts_path) {
                oldest_built = Some(oldest_built.map_or(built, |o| o.min(built)));
                if let Some(graph_mtime) = mtime_secs(&corpus_dir.join("scip_graph.db")) {
                    if graph_mtime > built {
                        any_lags_graph = true;
                    }
                }
            }

            if want("function") {
                for f in &facts.fn_defs {
                    if functions.len() >= limit * corpora.len() {
                        break;
                    }
                    if f.name.to_lowercase().contains(&lc) {
                        functions.push(json!({
                            "name": f.name, "file": f.file, "line": f.line, "corpus": corpus
                        }));
                    }
                }
            }
            if want("config") {
                for c in &facts.ctor_fields {
                    if config.len() >= limit * corpora.len() {
                        break;
                    }
                    if c.struct_type.to_lowercase().contains(&lc)
                        || c.field.to_lowercase().contains(&lc)
                        || c.value.to_lowercase().contains(&lc)
                    {
                        config.push(json!({
                            "struct_type": c.struct_type, "field": c.field, "value": c.value,
                            "enclosing_fn": c.enclosing_fn, "file": c.file, "line": c.line, "corpus": corpus
                        }));
                    }
                }
            }
            if want("literal") {
                for s in &facts.str_lits {
                    if literals.len() >= limit * corpora.len() {
                        break;
                    }
                    if s.content.to_lowercase().contains(&lc) {
                        literals.push(json!({
                            "content": s.content, "enclosing_fn": s.enclosing_fn,
                            "file": s.file, "line": s.line, "corpus": corpus
                        }));
                    }
                }
            }
        }

        let match_count = functions.len() + config.len() + literals.len();

        // No fact base anywhere we looked → honest `no_facts`, never a bare empty.
        if !any_facts_present {
            let hint = match corpus_id {
                Some(id) => format!(
                    "No fact base for corpus `{id}` at {}. Build it with \
                     `sovereign code facts <repo> --corpus-id {id}`.",
                    self.indexes_dir.join(id).join("facts.json").display()
                ),
                None => format!(
                    "No fact base found under {}. Build one with \
                     `sovereign code facts <repo> --corpus-id <id>`.",
                    self.indexes_dir.display()
                ),
            };
            return Ok(StepOutput::Json(json!({
                "status": "no_facts",
                "query": query,
                "corpora_searched": corpora,
                "match_count": 0,
                "freshness": freshness_block(None, false, now_unix()),
                "functions": [], "config": [], "literals": [],
                "hint": hint,
            })));
        }

        let status = if match_count == 0 { "no_matches" } else { "ok" };
        Ok(StepOutput::Json(json!({
            "status": status,
            "query": query,
            "corpora_searched": corpora,
            "match_count": match_count,
            "freshness": freshness_block(oldest_built, any_lags_graph, now_unix()),
            "functions": functions,
            "config": config,
            "literals": literals,
        })))
    }
}

/// Coarse absolute-age band. `lags_graph` is the sharper signal — the code
/// graph moved after these facts were cut — and forces `stale` regardless of
/// wall-clock age. Pure so the policy is unit-tested without touching mtimes.
fn staleness_of(age_hours: f64, lags_graph: bool) -> &'static str {
    if lags_graph || age_hours >= 168.0 {
        "stale"
    } else if age_hours >= 24.0 {
        "aging"
    } else {
        "fresh"
    }
}

/// Build the freshness stamp from the oldest fact base's build time and whether
/// any searched corpus's graph is newer than its facts. `now` is injected so
/// the whole function is pure and testable.
fn freshness_block(built_at_unix: Option<i64>, lags_graph: bool, now: i64) -> serde_json::Value {
    match built_at_unix {
        None => json!({
            "built_at_unix": null, "age_hours": null,
            "staleness": "unknown", "lags_graph": false,
            "note": "no fact base read",
        }),
        Some(built) => {
            let age_hours = (now - built).max(0) as f64 / 3600.0;
            let staleness = staleness_of(age_hours, lags_graph);
            let note = if lags_graph {
                "the code graph was rebuilt after these facts were extracted — a recent change may not be reflected; rebuild with `sovereign code facts`".to_string()
            } else {
                format!("fact base is ~{:.0}h old", age_hours)
            };
            json!({
                "built_at_unix": built,
                "age_hours": (age_hours * 10.0).round() / 10.0,
                "staleness": staleness,
                "lags_graph": lags_graph,
                "note": note,
            })
        }
    }
}

fn mtime_secs(path: &Path) -> Option<i64> {
    let meta = std::fs::metadata(path).ok()?;
    let mtime = meta.modified().ok()?;
    let dur = mtime.duration_since(std::time::UNIX_EPOCH).ok()?;
    Some(dur.as_secs() as i64)
}

fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use corpus_engine::facts::{CtorField, FnDef, StrLit};

    fn write_facts(dir: &Path, corpus: &str, facts: &Facts) -> PathBuf {
        let cdir = dir.join(corpus);
        std::fs::create_dir_all(&cdir).unwrap();
        let p = cdir.join("facts.json");
        facts.write(&p).unwrap();
        p
    }

    fn sample() -> Facts {
        Facts {
            fn_defs: vec![
                FnDef { name: "export_changed".into(), file: "scip.rs".into(), line: 300 },
                FnDef { name: "replace_files".into(), file: "graph.rs".into(), line: 1300 },
            ],
            ctor_fields: vec![CtorField {
                struct_type: "CodeWatcher".into(),
                field: "debounce".into(),
                value: "Duration::from_millis(800)".into(),
                enclosing_fn: "new".into(),
                file: "watch.rs".into(),
                line: 75,
            }],
            str_lits: vec![StrLit {
                content: "never_run".into(),
                enclosing_fn: "status".into(),
                file: "lint.rs".into(),
                line: 42,
            }],
        }
    }

    fn ctx() -> ToolContext {
        ToolContext {
            conversation_id: "facts-tool-test".into(),
            task_id: None,
            working_directory: None,
            in_reasoning_loop: false,
            agent_session_token: None,
            turn_index: 0,
        }
    }

    async fn run(tool: &FactsTool, params: serde_json::Value) -> serde_json::Value {
        match tool.execute(&params, &ctx()).await.unwrap() {
            StepOutput::Json(v) => v,
            other => panic!("expected Json, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn no_facts_is_honest_not_empty() {
        let tmp = tempfile::tempdir().unwrap();
        let tool = FactsTool::new(tmp.path());
        let out = run(&tool, json!({ "query": "anything" })).await;
        assert_eq!(out["status"], "no_facts");
        assert!(out["hint"].as_str().unwrap().contains("sovereign code facts"));
    }

    #[tokio::test]
    async fn finds_function_by_name() {
        let tmp = tempfile::tempdir().unwrap();
        write_facts(tmp.path(), "demo", &sample());
        let tool = FactsTool::new(tmp.path());
        let out = run(&tool, json!({ "query": "export_changed", "kind": "function" })).await;
        assert_eq!(out["status"], "ok");
        let fns = out["functions"].as_array().unwrap();
        assert_eq!(fns.len(), 1);
        assert_eq!(fns[0]["name"], "export_changed");
        assert_eq!(fns[0]["line"], 300);
        assert_eq!(fns[0]["corpus"], "demo");
    }

    #[tokio::test]
    async fn config_matches_field_and_value() {
        let tmp = tempfile::tempdir().unwrap();
        write_facts(tmp.path(), "demo", &sample());
        let tool = FactsTool::new(tmp.path());
        let out = run(&tool, json!({ "query": "debounce", "kind": "config" })).await;
        assert_eq!(out["match_count"], 1);
        assert_eq!(out["config"][0]["value"], "Duration::from_millis(800)");
    }

    #[tokio::test]
    async fn kind_filter_excludes_other_kinds() {
        let tmp = tempfile::tempdir().unwrap();
        write_facts(tmp.path(), "demo", &sample());
        let tool = FactsTool::new(tmp.path());
        // Query matches a literal, but we asked for functions only.
        let out = run(&tool, json!({ "query": "never_run", "kind": "function" })).await;
        assert_eq!(out["status"], "no_matches");
        assert_eq!(out["literals"].as_array().unwrap().len(), 0);
    }

    // ── Freshness policy (pure — no mtime plumbing) ──

    #[test]
    fn staleness_bands() {
        assert_eq!(staleness_of(1.0, false), "fresh");
        assert_eq!(staleness_of(23.9, false), "fresh");
        assert_eq!(staleness_of(24.0, false), "aging");
        assert_eq!(staleness_of(167.0, false), "aging");
        assert_eq!(staleness_of(168.0, false), "stale");
        // lags_graph forces stale regardless of age — the sharp signal.
        assert_eq!(staleness_of(0.5, true), "stale");
    }

    #[test]
    fn freshness_block_reports_age_and_lag() {
        // Built 48h ago, graph has NOT moved → aging, lags_graph false.
        let now = 1_000_000_i64;
        let built = now - 48 * 3600;
        let fb = freshness_block(Some(built), false, now);
        assert_eq!(fb["staleness"], "aging");
        assert_eq!(fb["lags_graph"], false);
        assert_eq!(fb["age_hours"], 48.0);

        // Same age but graph moved after facts were cut → stale + honest note.
        let fb2 = freshness_block(Some(built), true, now);
        assert_eq!(fb2["staleness"], "stale");
        assert_eq!(fb2["lags_graph"], true);
        assert!(fb2["note"].as_str().unwrap().contains("rebuilt after"));
    }

    #[test]
    fn freshness_block_unknown_when_no_facts() {
        let fb = freshness_block(None, false, 1_000_000);
        assert_eq!(fb["staleness"], "unknown");
        assert!(fb["built_at_unix"].is_null());
    }
}
