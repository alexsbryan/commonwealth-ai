// SPDX-License-Identifier: AGPL-3.0-or-later
//! Pure substrate tests: parse + DAG topo (incl. cycle), templating (incl. the
//! glassbox missing-key → empty), and the Runner threading artifacts through a
//! transform + a mock tool — no daemon, no weights, no MCP.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use sovereign_core::error::Result as CoreResult;
use sovereign_core::registry::ToolRegistry;
use sovereign_core::traits::Tool;
use sovereign_core::types::{
    Effect, Idempotency, Latency, Permission, Scope as ToolScope, StepOutput, ToolContext,
    ToolDescriptor,
};

use sovereign_workflow::model::{Artifact, Scope};
use sovereign_workflow::{template, Runner, StepRegistry, Workflow};

// ── A mock tool: echoes its `text` param ──────────────────────

struct EchoTool;

#[async_trait]
impl Tool for EchoTool {
    fn descriptor(&self) -> ToolDescriptor {
        ToolDescriptor {
            id: "echo".to_string(),
            name: "echo".to_string(),
            description: "echo the `text` param back as text".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": { "text": { "type": "string" } }
            }),
            examples: vec![],
            effect: Effect::Read,
            idempotency: Idempotency::Idempotent,
            latency: Latency::Fast,
            scope: ToolScope::Session,
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
    ) -> CoreResult<StepOutput> {
        let text = params
            .get("text")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        Ok(StepOutput::Text(text))
    }
}

fn echo_registry() -> Arc<ToolRegistry> {
    let mut tools = ToolRegistry::new();
    tools.register(Box::new(EchoTool));
    Arc::new(tools)
}

// ── parse + topo ──────────────────────────────────────────────

#[test]
fn topo_orders_a_chain_from_references() {
    let wf = Workflow::parse(
        r#"
[workflow]
name = "chain"
[[step]]
id = "c"
uses = "transform:identity"
input = "{b.output}"
[[step]]
id = "a"
uses = "transform:upper"
input = "x"
[[step]]
id = "b"
uses = "transform:identity"
input = "{a.output}"
"#,
    )
    .unwrap();
    let order = wf.topo_order().unwrap();
    let ids: Vec<&str> = order.iter().map(|&i| wf.steps[i].id.as_str()).collect();
    // a (no deps) before b before c, regardless of authored order.
    assert!(ids.iter().position(|&x| x == "a") < ids.iter().position(|&x| x == "b"));
    assert!(ids.iter().position(|&x| x == "b") < ids.iter().position(|&x| x == "c"));
}

#[test]
fn cycle_is_a_loud_error() {
    let wf = Workflow::parse(
        r#"
[workflow]
name = "loop"
[[step]]
id = "a"
uses = "transform:identity"
input = "{b.output}"
[[step]]
id = "b"
uses = "transform:identity"
input = "{a.output}"
"#,
    )
    .unwrap();
    let err = wf.topo_order().unwrap_err().to_string();
    assert!(err.contains("cycle"), "{err}");
}

#[test]
fn duplicate_step_id_rejected() {
    let err = Workflow::parse(
        r#"
[workflow]
name = "dup"
[[step]]
id = "a"
uses = "transform:identity"
[[step]]
id = "a"
uses = "transform:upper"
"#,
    )
    .unwrap_err()
    .to_string();
    assert!(err.contains("duplicate"), "{err}");
}

// ── step-kind taxonomy (type-safe dispatch, ARCH §2.1) ────────

#[test]
fn step_kind_parse_and_resource_classification() {
    use sovereign_core::oicp::LatencyClass;
    use sovereign_workflow::{ResourceNeed, StepKind};

    // Parse maps the wire form to the typed variant — the one boundary.
    // A `model:` slot is OICP's own `LatencyClass` vocabulary.
    assert_eq!(
        StepKind::parse("model:fast").unwrap(),
        StepKind::Model {
            latency: LatencyClass::Fast
        }
    );
    assert_eq!(
        StepKind::parse("model:extended").unwrap(),
        StepKind::Model {
            latency: LatencyClass::Extended
        }
    );
    // `thoughtful` / `slow` are friendly aliases for `extended`.
    assert_eq!(
        StepKind::parse("model:thoughtful").unwrap(),
        StepKind::Model {
            latency: LatencyClass::Extended
        }
    );
    assert_eq!(
        StepKind::parse("embed:default").unwrap(),
        StepKind::Embed {
            model: "default".into()
        }
    );
    assert_eq!(
        StepKind::parse("mcp:demo:read_memo").unwrap(),
        StepKind::Mcp {
            server: "demo".into(),
            tool: "read_memo".into()
        }
    );

    // resources() is the single classifier the command + scheduler read —
    // pinning it here means a new variant can't quietly mis-declare its need.
    let r = |u: &str| StepKind::parse(u).unwrap().resources();
    assert_eq!(r("model:thoughtful"), ResourceNeed::Inference);
    assert_eq!(r("embed:default"), ResourceNeed::Inference);
    assert_eq!(r("tool:write_note"), ResourceNeed::Tool);
    assert_eq!(r("mcp:demo:read"), ResourceNeed::Tool);
    assert_eq!(r("transform:upper"), ResourceNeed::None);

    // Malformed forms are loud errors, not silent misroutes.
    assert!(StepKind::parse("noColon").is_err());
    assert!(StepKind::parse("bogus:x").is_err());
    assert!(StepKind::parse("mcp:missing_tool").is_err());
    assert!(StepKind::parse("model:turbo").is_err()); // unknown latency, not a silent default
}

// ── templating ────────────────────────────────────────────────

#[test]
fn resolve_substitutes_item_and_step_refs_and_warns_on_missing() {
    let mut scope = Scope::default();
    scope.item.insert("path".into(), "/notes/a.md".into());
    scope.completed.insert(
        "read".into(),
        Artifact {
            type_tag: "text".into(),
            output: StepOutput::Text("BODY".into()),
        },
    );
    let out = template::resolve_str("{item.path} :: {read.output} :: {gone.key}", &scope);
    // item field + completed step resolve; an unknown ref → empty string.
    assert_eq!(out, "/notes/a.md :: BODY :: ");
}

// ── runner end-to-end (transform + mock tool) ─────────────────

#[tokio::test]
async fn runner_threads_artifacts_per_item() {
    let wf = Workflow::parse(
        r#"
[workflow]
name = "echo-upper"

[source]
type = "inline"
items = ["alpha", "beta"]

[[step]]
id = "up"
uses = "transform:upper"
input = "{item.name}"

[[step]]
id = "echo"
uses = "tool:echo"
params = { text = "[{up.output}]" }
"#,
    )
    .unwrap();

    let registry = StepRegistry::new(None, echo_registry());
    let report = Runner::new(registry).run(&wf, 2).await.unwrap();

    assert_eq!(report.ok_count(), 2);
    assert_eq!(report.failed_count(), 0);
    let mut finals: Vec<String> = report
        .items
        .iter()
        .map(|i| i.result.as_ref().unwrap().clone())
        .collect();
    finals.sort();
    // each item flowed item.name → upper → echo([UPPER])
    assert_eq!(finals, vec!["[ALPHA]".to_string(), "[BETA]".to_string()]);
}

#[tokio::test]
async fn model_step_without_a_daemon_errors_clearly() {
    let wf = Workflow::parse(
        r#"
[workflow]
name = "needs-model"
[[step]]
id = "m"
uses = "model:thoughtful"
prompt = "hi"
"#,
    )
    .unwrap();
    // No inference provider wired → resolving the model step must error, not panic.
    let registry = StepRegistry::new(None, Arc::new(ToolRegistry::new()));
    let err = Runner::new(registry).run(&wf, 1).await.unwrap_err().to_string();
    assert!(err.contains("daemon"), "{err}");
}

// ── content-addressed cache ───────────────────────────────────

#[test]
fn cache_key_is_stable_and_input_sensitive() {
    use sovereign_workflow::cache::cache_key;
    use sovereign_workflow::ResolvedArgs;

    let a = ResolvedArgs {
        input: Some("x".into()),
        ..Default::default()
    };
    let k1 = cache_key("transform:upper", "up", &a, "fp1");
    assert_eq!(k1, cache_key("transform:upper", "up", &a, "fp1"), "stable");

    let b = ResolvedArgs {
        input: Some("y".into()),
        ..Default::default()
    };
    assert_ne!(k1, cache_key("transform:upper", "up", &b, "fp1"), "args change");
    assert_ne!(k1, cache_key("transform:upper", "up", &a, "fp2"), "fingerprint change");
}

#[tokio::test]
async fn read_step_is_cached_on_rerun() {
    let dir = tempfile::tempdir().unwrap();
    let cache: Arc<dyn sovereign_workflow::ArtifactCache> =
        Arc::new(sovereign_workflow::FileArtifactCache::new(dir.path().to_path_buf()));
    let toml = r#"
[workflow]
name = "cache-me"
[source]
type = "inline"
items = ["alpha"]
[[step]]
id = "up"
uses = "transform:upper"
input = "{item.name}"
"#;
    let wf = Workflow::parse(toml).unwrap();

    let r1 = Runner::with_cache(StepRegistry::new(None, Arc::new(ToolRegistry::new())), cache.clone())
        .run(&wf, 1)
        .await
        .unwrap();
    assert_eq!((r1.ran_total(), r1.cached_total()), (1, 0), "first run runs");

    let r2 = Runner::with_cache(StepRegistry::new(None, Arc::new(ToolRegistry::new())), cache.clone())
        .run(&wf, 1)
        .await
        .unwrap();
    assert_eq!((r2.ran_total(), r2.cached_total()), (0, 1), "re-run is fully cached");
}

struct WriteCounterTool {
    calls: Arc<AtomicUsize>,
}

#[async_trait]
impl Tool for WriteCounterTool {
    fn descriptor(&self) -> ToolDescriptor {
        ToolDescriptor {
            id: "wc".to_string(),
            name: "wc".to_string(),
            description: "a write-effect tool".to_string(),
            parameters: serde_json::json!({}),
            examples: vec![],
            effect: Effect::Write,
            idempotency: Idempotency::NonIdempotent,
            latency: Latency::Fast,
            scope: ToolScope::Persistent,
            output_schema: None,
        }
    }
    fn required_permissions(&self) -> Vec<Permission> {
        vec![]
    }
    async fn execute(
        &self,
        _params: &serde_json::Value,
        _ctx: &ToolContext,
    ) -> CoreResult<StepOutput> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(StepOutput::Text("wrote".into()))
    }
}

#[tokio::test]
async fn write_step_is_never_cached() {
    let dir = tempfile::tempdir().unwrap();
    let cache: Arc<dyn sovereign_workflow::ArtifactCache> =
        Arc::new(sovereign_workflow::FileArtifactCache::new(dir.path().to_path_buf()));
    let calls = Arc::new(AtomicUsize::new(0));
    let mut tools = ToolRegistry::new();
    tools.register(Box::new(WriteCounterTool {
        calls: Arc::clone(&calls),
    }));
    let tools = Arc::new(tools);

    let wf = Workflow::parse(
        r#"
[workflow]
name = "writes"
[source]
type = "inline"
items = ["a"]
[[step]]
id = "w"
uses = "tool:wc"
"#,
    )
    .unwrap();

    for _ in 0..2 {
        let r = Runner::with_cache(
            StepRegistry::new(None, Arc::clone(&tools)),
            cache.clone(),
        )
        .run(&wf, 1)
        .await
        .unwrap();
        // A Write step is never cached — it always runs (the side effect must happen).
        assert_eq!(r.cached_total(), 0);
    }
    assert_eq!(calls.load(Ordering::SeqCst), 2, "write ran both times, never cached");
}

// ── for_each fan-out (collection → map) ───────────────────────

/// A `1→N` step: splits its `text` param on whitespace into a JSON array of
/// `{text, index}` objects — a chunker stand-in, the shape a `for_each` maps over.
struct SplitTool;

#[async_trait]
impl Tool for SplitTool {
    fn descriptor(&self) -> ToolDescriptor {
        ToolDescriptor {
            id: "split".to_string(),
            name: "split".to_string(),
            description: "split `text` into a collection of {text, index}".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": { "text": { "type": "string" } }
            }),
            examples: vec![],
            effect: Effect::Read,
            idempotency: Idempotency::Idempotent,
            latency: Latency::Fast,
            scope: ToolScope::Session,
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
    ) -> CoreResult<StepOutput> {
        let text = params.get("text").and_then(|v| v.as_str()).unwrap_or("");
        let arr: Vec<serde_json::Value> = text
            .split_whitespace()
            .enumerate()
            .map(|(i, w)| serde_json::json!({ "text": w, "index": i }))
            .collect();
        Ok(StepOutput::Json(serde_json::Value::Array(arr)))
    }
}

fn split_registry() -> Arc<ToolRegistry> {
    let mut tools = ToolRegistry::new();
    tools.register(Box::new(SplitTool));
    Arc::new(tools)
}

#[tokio::test]
async fn for_each_maps_a_step_over_a_collection() {
    // `split` (1→N) hands a JSON-array collection to `up`, which maps over each
    // element via `for_each` — the fan-out the Runner grew. The Artifact never
    // changed: a collection is just a JSON array.
    let wf = Workflow::parse(
        r#"
[workflow]
name = "split-map"
[source]
type = "inline"
items = ["alpha beta gamma"]
[[step]]
id = "split"
uses = "tool:split"
params = { text = "{item.name}" }
[[step]]
id = "up"
uses = "transform:upper"
for_each = "split"
input = "{element.text}"
"#,
    )
    .unwrap();

    let report = Runner::new(StepRegistry::new(None, split_registry()))
        .run(&wf, 1)
        .await
        .unwrap();

    assert_eq!(report.ok_count(), 1);
    // The for_each step's output is the JSON array of per-element results.
    let final_text = report.items[0].result.as_ref().unwrap();
    let arr: serde_json::Value = serde_json::from_str(final_text).unwrap();
    assert_eq!(arr, serde_json::json!(["ALPHA", "BETA", "GAMMA"]));
}

#[tokio::test]
async fn for_each_caches_per_element() {
    let dir = tempfile::tempdir().unwrap();
    let cache: Arc<dyn sovereign_workflow::ArtifactCache> =
        Arc::new(sovereign_workflow::FileArtifactCache::new(dir.path().to_path_buf()));

    let toml = |items: &str| {
        format!(
            r#"
[workflow]
name = "split-map-cache"
[source]
type = "inline"
items = [{items}]
[[step]]
id = "split"
uses = "tool:split"
params = {{ text = "{{item.name}}" }}
[[step]]
id = "up"
uses = "transform:upper"
for_each = "split"
input = "{{element.text}}"
"#
        )
    };

    // Run 1 — split runs (1) + 3 element maps = 4 ran, 0 cached.
    let wf1 = Workflow::parse(&toml(r#""a b c""#)).unwrap();
    let r1 = Runner::with_cache(StepRegistry::new(None, split_registry()), cache.clone())
        .run(&wf1, 1)
        .await
        .unwrap();
    assert_eq!((r1.ran_total(), r1.cached_total()), (4, 0), "first run runs all");

    // Run 2, identical — everything is a cache hit.
    let r2 = Runner::with_cache(StepRegistry::new(None, split_registry()), cache.clone())
        .run(&wf1, 1)
        .await
        .unwrap();
    assert_eq!((r2.ran_total(), r2.cached_total()), (0, 4), "re-run fully cached");

    // Run 3, edit ONE element (c → X): `split` re-runs (its input changed), and
    // only the changed element re-maps — `a` and `b` are reused. This is the
    // per-element granularity: editing one chunk re-embeds only that chunk.
    let wf3 = Workflow::parse(&toml(r#""a b X""#)).unwrap();
    let r3 = Runner::with_cache(StepRegistry::new(None, split_registry()), cache.clone())
        .run(&wf3, 1)
        .await
        .unwrap();
    assert_eq!(
        (r3.ran_total(), r3.cached_total()),
        (2, 2),
        "only split + the one changed element re-ran"
    );
}
