// SPDX-License-Identifier: AGPL-3.0-or-later
//! Pure substrate tests: parse + DAG topo (incl. cycle), templating (incl. the
//! glassbox missing-key → empty), and the Runner threading artifacts through a
//! transform + a mock tool — no daemon, no weights, no MCP.

use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use futures::Stream;
use sovereign_core::error::Result as CoreResult;
use sovereign_core::registry::ToolRegistry;
use sovereign_core::traits::{InferenceProvider, Tool};
use sovereign_core::types::{
    CompletionRequest, CompletionResponse, Depth, Effect, Idempotency, Latency, Permission,
    ProviderCapabilities, Scope as ToolScope, Speed, StepOutput, ToolContext, ToolDescriptor,
};

use sovereign_workflow::model::{Artifact, Scope};
use sovereign_workflow::{
    template, Runner, StepObserver, StepRegistry, Workflow, WorkflowProgress,
};

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

// ── progress observer (the glassbox seam the run surface streams) ──

#[tokio::test]
async fn observer_sees_every_lifecycle_event_in_order() {
    // Two deterministic transforms, no source → one item, two steps. No
    // provider/tools needed, so this exercises the observer hermetically.
    let wf = Workflow::parse(
        r#"
[workflow]
name = "obs"
[[step]]
id = "a"
uses = "transform:upper"
input = "hello"
[[step]]
id = "b"
uses = "transform:identity"
input = "{a.output}"
"#,
    )
    .unwrap();

    let events: Arc<Mutex<Vec<WorkflowProgress>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&events);
    let observer: StepObserver = Arc::new(move |ev| sink.lock().unwrap().push(ev));

    let registry = StepRegistry::new(None, Arc::new(ToolRegistry::new()));
    let report = Runner::new(registry)
        .with_observer(Some(observer))
        .run(&wf, 1)
        .await
        .unwrap();
    assert_eq!(report.ok_count(), 1);

    let evs = events.lock().unwrap();
    assert_eq!(evs.len(), 5, "got {evs:?}");
    assert!(matches!(
        evs[0],
        WorkflowProgress::RunStarted {
            items: 1,
            steps: 2,
            ..
        }
    ));
    match &evs[1] {
        WorkflowProgress::StepDone {
            step,
            step_index: 0,
            total_steps: 2,
            ..
        } => assert_eq!(step, "a"),
        other => panic!("expected StepDone(a), got {other:?}"),
    }
    match &evs[2] {
        WorkflowProgress::StepDone {
            step,
            step_index: 1,
            total_steps: 2,
            ..
        } => assert_eq!(step, "b"),
        other => panic!("expected StepDone(b), got {other:?}"),
    }
    assert!(matches!(evs[3], WorkflowProgress::ItemDone { ok: true, .. }));
    assert!(matches!(
        evs[4],
        WorkflowProgress::RunFinished { ok: 1, failed: 0 }
    ));
}

#[test]
fn referenced_params_collects_every_param_key() {
    // `{param.*}` in the source path/glob AND in a step's params field — the
    // run surface turns each into a form field.
    let wf = Workflow::parse(
        r#"
[workflow]
name = "params"
[source]
type = "folder"
path = "{param.folder}"
glob = "{param.glob}"
[[step]]
id = "store"
uses = "transform:identity"
input = "x"
params = { corpus = "{param.corpus}" }
"#,
    )
    .unwrap();
    let params: Vec<String> = wf.referenced_params().into_iter().collect();
    // BTreeSet → sorted + de-duplicated.
    assert_eq!(params, vec!["corpus", "folder", "glob"]);
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
    std::sync::Arc::make_mut(&mut scope.completed).insert(
        "read".into(),
        Artifact::new("text", StepOutput::Text("BODY".into())),
    );
    let out = template::resolve_str("{item.path} :: {read.output} :: {gone.key}", &scope);
    // item field + completed step resolve; an unknown ref → empty string.
    assert_eq!(out, "/notes/a.md :: BODY :: ");
}

#[test]
fn model_step_carries_structured_output_and_grammar() {
    // A general `model:` primitive: a step declares its output shape (a JSON
    // schema) and/or a grammar as data, so an extraction constrains the model in
    // TOML rather than parsing free text.
    let wf = Workflow::parse(
        r#"
[workflow]
name = "structured"
[[step]]
id = "extract"
uses = "model:fast"
prompt = "{item.name}"
grammar = "start: object"
structured_output = { type = "object", required = ["atoms"] }
"#,
    )
    .unwrap();
    let args = template::resolve_args(&wf.steps[0], &Scope::default());
    assert_eq!(args.grammar.as_deref(), Some("start: object"));
    let so = args.structured_output.expect("structured_output resolved");
    assert_eq!(so.get("type").and_then(|v| v.as_str()), Some("object"));
    assert_eq!(
        so.get("required").and_then(|v| v.as_array()).map(|a| a.len()),
        Some(1)
    );
}

/// A model that always returns the given text — to prove a structured step
/// parses it into a `Json` artifact.
struct CannedModel(&'static str);

#[async_trait]
impl InferenceProvider for CannedModel {
    async fn complete(&self, _req: &CompletionRequest) -> CoreResult<CompletionResponse> {
        Ok(CompletionResponse {
            text: self.0.to_string(),
            tokens_used: 0,
            prompt_tokens: 0,
            model_id: "canned".into(),
            latency_ms: 0,
            oicp_meta: None,
            finish_reason: None,
            completion_tokens: None,
        })
    }
    async fn complete_stream(
        &self,
        _req: &CompletionRequest,
    ) -> CoreResult<Pin<Box<dyn Stream<Item = CoreResult<String>> + Send>>> {
        unreachable!("structured-output test never streams")
    }
    async fn embed(&self, _text: &str) -> CoreResult<Vec<f32>> {
        Ok(vec![])
    }
    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            max_context_tokens: 8192,
            supports_structured_output: true,
            relative_speed: Speed::Fast,
            relative_reasoning: Depth::Shallow,
        }
    }
}

#[tokio::test]
async fn structured_model_output_is_a_parsed_json_artifact() {
    // The model returns a JSON object; because the step declared
    // `structured_output`, the runner parses it into a `Json` artifact — so a
    // downstream `{x.questions}` resolves to the *field*, not the whole string.
    // (A `Text` artifact would yield the entire object via `{x.questions}`.)
    let provider: Arc<dyn InferenceProvider> = Arc::new(CannedModel(r#"{"questions":["a","b"]}"#));
    let registry = StepRegistry::new(Some(provider), Arc::new(ToolRegistry::new()));
    let wf = Workflow::parse(
        r#"
[workflow]
name = "structured"
[[step]]
id = "x"
uses = "model:fast"
prompt = "go"
structured_output = { type = "object" }
[[step]]
id = "pick"
uses = "transform:identity"
input = "{x.questions}"
"#,
    )
    .unwrap();
    let report = Runner::new(registry).run(&wf, 1).await.unwrap();
    assert_eq!(report.ok_count(), 1, "{:?}", report.items);
    let picked = report.items[0].result.as_ref().unwrap();
    let arr: serde_json::Value = serde_json::from_str(picked).unwrap();
    assert_eq!(arr, serde_json::json!(["a", "b"]));
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

/// Records peak concurrent in-flight calls; echoes `text`. A 30ms hold makes
/// overlap near-certain when the runner maps elements concurrently.
struct ConcurrencyProbeTool {
    inflight: Arc<AtomicUsize>,
    peak: Arc<AtomicUsize>,
}

#[async_trait]
impl Tool for ConcurrencyProbeTool {
    fn descriptor(&self) -> ToolDescriptor {
        ToolDescriptor {
            id: "probe".to_string(),
            name: "probe".to_string(),
            description: "record peak concurrency; echo `text`".to_string(),
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
        let now = self.inflight.fetch_add(1, Ordering::SeqCst) + 1;
        self.peak.fetch_max(now, Ordering::SeqCst);
        tokio::time::sleep(std::time::Duration::from_millis(30)).await;
        self.inflight.fetch_sub(1, Ordering::SeqCst);
        let text = params
            .get("text")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        Ok(StepOutput::Text(text))
    }
}

#[tokio::test]
async fn for_each_runs_elements_concurrently_and_in_order() {
    let inflight = Arc::new(AtomicUsize::new(0));
    let peak = Arc::new(AtomicUsize::new(0));
    let mut tools = ToolRegistry::new();
    tools.register(Box::new(SplitTool));
    tools.register(Box::new(ConcurrencyProbeTool {
        inflight: Arc::clone(&inflight),
        peak: Arc::clone(&peak),
    }));
    let registry = StepRegistry::new(None, Arc::new(tools));

    let wf = Workflow::parse(
        r#"
[workflow]
name = "concurrent-map"
[source]
type = "inline"
items = ["a b c d e f"]
[[step]]
id = "split"
uses = "tool:split"
params = { text = "{item.name}" }
[[step]]
id = "probe"
uses = "tool:probe"
for_each = "split"
params = { text = "{element.text}" }
"#,
    )
    .unwrap();

    let report = Runner::new(registry).run(&wf, 4).await.unwrap();
    assert_eq!(report.ok_count(), 1, "{:?}", report.items);

    // 6 elements at concurrency 4 → peak in-flight must exceed 1.
    assert!(
        peak.load(Ordering::SeqCst) >= 2,
        "for_each must run elements concurrently; peak in-flight = {}",
        peak.load(Ordering::SeqCst)
    );

    // Order is preserved despite concurrent execution.
    let arr: serde_json::Value =
        serde_json::from_str(report.items[0].result.as_ref().unwrap()).unwrap();
    assert_eq!(arr, serde_json::json!(["a", "b", "c", "d", "e", "f"]));
}

/// Every shipped example workflow parses — a guard against example rot (a TOML
/// typo, or a primitive renamed without updating the docs/demos). Parse-only: it
/// validates structure (`[[step]]` shape, unique ids), not live resolution.
#[test]
fn shipped_examples_parse() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("examples");
    let mut parsed = Vec::new();
    for entry in std::fs::read_dir(&dir).expect("examples dir exists").flatten() {
        let p = entry.path();
        if p.extension().and_then(|e| e.to_str()) != Some("toml") {
            continue;
        }
        let toml = std::fs::read_to_string(&p).unwrap();
        sovereign_workflow::Workflow::parse(&toml)
            .unwrap_or_else(|e| panic!("example {} must parse: {e}", p.display()));
        parsed.push(p.file_name().unwrap().to_string_lossy().into_owned());
    }
    assert!(
        parsed.len() >= 3,
        "expected several shipped example workflows, found {parsed:?}"
    );
}

/// Upper-cases its `text`, or errors on the marker "BOOM" — drives the
/// `for_each` error-tolerance tests.
struct FlakyTool;

#[async_trait]
impl Tool for FlakyTool {
    fn descriptor(&self) -> ToolDescriptor {
        ToolDescriptor {
            id: "flaky".to_string(),
            name: "flaky".to_string(),
            description: "upper-case `text`, or error on BOOM".to_string(),
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
        if text == "BOOM" {
            return Err(sovereign_core::error::Error::Execution("kaboom".into()));
        }
        Ok(StepOutput::Text(text.to_uppercase()))
    }
}

fn flaky_registry() -> Arc<ToolRegistry> {
    let mut tools = ToolRegistry::new();
    tools.register(Box::new(SplitTool));
    tools.register(Box::new(FlakyTool));
    Arc::new(tools)
}

/// `on_error = "skip"`: a `for_each` whose element fails records it in the step's
/// `failures` and continues with the rest — the real Phase-1 skip-and-continue.
/// The envelope then captures `{proc.output}` (successes) and `{proc.failures}`
/// as data, so a flaky element stays visible rather than silently lost, and a
/// single failure never aborts the whole run.
#[tokio::test]
async fn for_each_on_error_skip_records_failures_and_continues() {
    let wf = Workflow::parse(
        r#"
[workflow]
name = "skip-failures"
[source]
type = "inline"
items = ["alpha BOOM gamma"]
[[step]]
id = "split"
uses = "tool:split"
params = { text = "{item.name}" }
[[step]]
id = "proc"
uses = "tool:flaky"
for_each = "split"
on_error = "skip"
params = { text = "{element.text}" }
[[step]]
id = "env"
uses = "transform:json"
params = { ok = "{proc.output}", failed = "{proc.failures}" }
"#,
    )
    .unwrap();

    let report = Runner::new(StepRegistry::new(None, flaky_registry()))
        .run(&wf, 4)
        .await
        .unwrap();
    assert_eq!(
        report.ok_count(),
        1,
        "the run survives the failing element: {:?}",
        report.items
    );

    let env: serde_json::Value =
        serde_json::from_str(report.items[0].result.as_ref().unwrap()).unwrap();
    // Successes only — the BOOM element is dropped from the output.
    assert_eq!(env["ok"], serde_json::json!(["ALPHA", "GAMMA"]));
    // The failure is recorded AS DATA: index 1 (BOOM's position) + the error.
    let failed = env["failed"].as_array().expect("failures is an array");
    assert_eq!(failed.len(), 1, "exactly one element failed: {failed:?}");
    assert_eq!(failed[0]["index"], serde_json::json!(1));
    assert!(failed[0]["error"].as_str().unwrap().contains("kaboom"));
}

/// The default (no `on_error`) is unchanged: the first element error aborts the
/// whole item — the safe default for a workflow that wants all-or-nothing.
#[tokio::test]
async fn for_each_default_aborts_on_element_error() {
    let wf = Workflow::parse(
        r#"
[workflow]
name = "abort-default"
[source]
type = "inline"
items = ["alpha BOOM gamma"]
[[step]]
id = "split"
uses = "tool:split"
params = { text = "{item.name}" }
[[step]]
id = "proc"
uses = "tool:flaky"
for_each = "split"
params = { text = "{element.text}" }
"#,
    )
    .unwrap();

    let report = Runner::new(StepRegistry::new(None, flaky_registry()))
        .run(&wf, 4)
        .await
        .unwrap();
    assert_eq!(
        report.failed_count(),
        1,
        "the default aborts the item on an element error"
    );
    assert!(report.items[0]
        .result
        .as_ref()
        .unwrap_err()
        .contains("kaboom"));
}

/// Echoes the system message it received back as the completion — lets a test
/// prove a `model:` step's `system_file` was loaded from disk.
struct EchoSystemProvider;

#[async_trait]
impl InferenceProvider for EchoSystemProvider {
    async fn complete(&self, req: &CompletionRequest) -> CoreResult<CompletionResponse> {
        Ok(CompletionResponse {
            text: req.system_message.clone().unwrap_or_default(),
            tokens_used: 0,
            prompt_tokens: 0,
            model_id: "echo-system".into(),
            latency_ms: 0,
            oicp_meta: None,
            finish_reason: None,
            completion_tokens: None,
        })
    }
    async fn complete_stream(
        &self,
        _req: &CompletionRequest,
    ) -> CoreResult<Pin<Box<dyn Stream<Item = CoreResult<String>> + Send>>> {
        unreachable!("system_file test never streams")
    }
    async fn embed(&self, _text: &str) -> CoreResult<Vec<f32>> {
        unreachable!("system_file test never embeds")
    }
    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            max_context_tokens: 8192,
            supports_structured_output: false,
            relative_speed: Speed::Fast,
            relative_reasoning: Depth::Shallow,
        }
    }
}

/// `system_file`: a `model:` step loads its (large, static) system prompt from a
/// file — the bespoke enrichment `.md` prompts referenced as data, not re-typed
/// inline — while the dynamic content stays in the templated `prompt`. The
/// foundational primitive for composing the LLM enrichment phases faithfully.
#[tokio::test]
async fn model_system_file_loads_the_system_prompt_from_disk() {
    let dir = tempfile::tempdir().unwrap();
    let prompt_path = dir.path().join("phase1a_seed_system.md");
    std::fs::write(&prompt_path, "YOU ARE A SEED EXTRACTOR. Respond ONLY with JSON.").unwrap();

    let toml = r#"
[workflow]
name = "system-file"
[source]
type = "inline"
items = ["chapter-one"]
[[step]]
id = "m"
uses = "model:fast"
system_file = "__PROMPT__"
prompt = "Extract seeds from {item.name}."
"#
    .replace("__PROMPT__", &prompt_path.to_string_lossy());

    let wf = Workflow::parse(&toml).unwrap();
    let registry = StepRegistry::new(
        Some(Arc::new(EchoSystemProvider) as Arc<dyn InferenceProvider>),
        Arc::new(ToolRegistry::new()),
    );
    let report = Runner::new(registry).run(&wf, 1).await.unwrap();
    assert_eq!(report.ok_count(), 1, "{:?}", report.items);
    // The model received the file's content verbatim as its system message.
    assert_eq!(
        report.items[0].result.as_ref().unwrap(),
        "YOU ARE A SEED EXTRACTOR. Respond ONLY with JSON."
    );
}
