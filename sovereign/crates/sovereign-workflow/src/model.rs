// SPDX-License-Identifier: AGPL-3.0-or-later
//! Workflow data model: the typed `Artifact`, the `StepDescriptor`, and the
//! `Workflow` graph (parsed from TOML, DAG edges auto-derived from `{step.key}`
//! references). Pure data — no inference or tool dependencies.

use std::cmp::Reverse;
use std::collections::{BTreeMap, BinaryHeap, HashSet};
use std::path::Path;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use sovereign_core::error::{Error, Result};
use sovereign_core::types::{Effect, StepOutput};

use crate::template;

/// A typed value flowing between steps. Serializable so the content-addressed
/// cache can persist it; P2's `id`/`lineage` (content hash + provenance edges)
/// fold in on top.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Artifact {
    pub type_tag: String,
    pub output: StepOutput,
    /// Per-element failures recorded by a tolerant `for_each` (`on_error =
    /// "skip"`): the elements that errored, so the run continues with the
    /// successes while the failures stay visible *as data* — the workflow analog
    /// of `Phase1Output.failures`. Empty for every ordinary step. Each entry is
    /// `{ index, error }`; referenced downstream as `{step.failures}`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub failures: Vec<serde_json::Value>,
}

impl Artifact {
    /// An artifact with no recorded failures — the common case (a step that
    /// either fully succeeds or aborts). Construct via this rather than a struct
    /// literal so adding artifact metadata never ripples across every step.
    pub fn new(type_tag: impl Into<String>, output: StepOutput) -> Self {
        Self {
            type_tag: type_tag.into(),
            output,
            failures: Vec::new(),
        }
    }

    /// Attach the per-element failures of a tolerant `for_each`.
    pub fn with_failures(mut self, failures: Vec<serde_json::Value>) -> Self {
        self.failures = failures;
        self
    }
}

/// What a step needs from the scheduler. P1 carries the kind; P2 carries the
/// `InferenceRequirements` / `Permission` payloads the scheduler + approval gate
/// consume.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResourceNeed {
    None,
    Inference,
    Tool,
    /// A `recipe:` stage — needs the corpus installer (delegates to the
    /// corpus-install path). Not the inference provider: the install does its own
    /// embedding via the daemon engine.
    Install,
}

/// A step's self-description. `effect` is now consumed by the content cache (a
/// `Read` step is safe to skip on a cache hit; a `Write` step must always run
/// so its side effect happens). The rest of the `ToolDescriptor` behavioural
/// metadata (idempotency/latency/scope/output_schema) folds in when the
/// scheduler needs it.
#[derive(Debug, Clone)]
pub struct StepDescriptor {
    pub id: String,
    pub name: String,
    pub description: String,
    pub resources: ResourceNeed,
    pub effect: Effect,
    pub deterministic: bool,
}

impl StepDescriptor {
    /// Safe to skip on a cache hit: it produces an output but no external side
    /// effect (a `Read` tool, a model call, a deterministic transform). A
    /// `Write`/`ReadWrite` step is never cached — skipping it would skip the
    /// write. A per-step `cache = false` opt-out (volatile reads like web fetch)
    /// is applied by the runner on top of this.
    pub fn cacheable(&self) -> bool {
        self.effect == Effect::Read
    }
}

/// Per-item resolution context threaded through one item's run.
///
/// `completed` is an `Arc` so a `for_each` step can clone the scope per element
/// in O(1) instead of copying the whole completed-artifact map — without it,
/// mapping over an N-element collection is O(N²) in the scope size. The runner
/// mutates it between steps via `Arc::make_mut` (a no-op clone since the element
/// scopes that share it are dropped before the next insert).
#[derive(Debug, Default, Clone)]
pub struct Scope {
    pub item: BTreeMap<String, String>,
    pub completed: Arc<BTreeMap<String, Artifact>>,
    /// The current element when resolving a `for_each` step — read via
    /// `{element.key}` (an object field) or `{element.value}` (a scalar).
    /// `None` outside a `for_each` body.
    pub element: Option<serde_json::Value>,
    /// Run-global author-supplied parameters, read via `{param.key}`. Unlike
    /// `item`/`element` these are the same for every item, supplied at run time
    /// (`--param k=v`, `--folder`). `Arc` so the per-element `for_each` scope
    /// clone stays O(1) like `completed`. Empty by default.
    pub params: Arc<BTreeMap<String, String>>,
}

/// A step's templated fields, resolved against the scope (see `template`).
/// Serialized into the cache key — the resolved values *are* the step's inputs.
#[derive(Debug, Clone, Default, Serialize)]
pub struct ResolvedArgs {
    pub prompt: Option<String>,
    pub system: Option<String>,
    pub system_file: Option<String>,
    pub input: Option<String>,
    pub params: Option<serde_json::Value>,
    pub structured_output: Option<serde_json::Value>,
    pub grammar: Option<String>,
    pub stamp: Option<serde_json::Value>,
    pub raw_output: bool,
}

/// Optional per-step scheduler hints (P2-bound; parsed in P1, mostly inert).
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Resources {
    #[serde(default)]
    pub latency_class: Option<String>,
    #[serde(default)]
    pub privacy: Option<String>,
}

/// One node in the workflow, as authored.
#[derive(Debug, Clone, Deserialize)]
pub struct StepSpec {
    pub id: String,
    pub uses: String,
    #[serde(default)]
    pub prompt: Option<String>,
    #[serde(default)]
    pub system: Option<String>,
    /// Load the system prompt from a file instead of inline `system` — so a
    /// `model:` step references a large prompt asset (the bespoke enrichment
    /// `.md` prompts) as data rather than re-typing it, which keeps the workflow
    /// byte-faithful to the prompt it's reproducing. The path is templated (so
    /// `{item.*}` can parameterise it); the file content is used verbatim as the
    /// system message (the dynamic content goes in the templated `prompt`).
    /// Overrides `system` when both are set.
    #[serde(default)]
    pub system_file: Option<String>,
    #[serde(default)]
    pub input: Option<String>,
    #[serde(default)]
    pub params: Option<toml::Value>,
    #[serde(default)]
    pub resources: Option<Resources>,
    /// Cache opt-out for a volatile `Read` step (e.g. a web fetch whose target
    /// changes over time). `Some(false)` → always run, never cache. Default
    /// (`None`) → cache iff the step's effect is `Read`.
    #[serde(default)]
    pub cache: Option<bool>,
    /// Map this step over the collection produced by another step (whose output
    /// is a JSON array). The step runs once per element, read via `{element.key}`
    /// (object field) or `{element.value}` (scalar); its output is the JSON array
    /// of the per-element results. This is how a `1→N` stage (a chunker) hands
    /// off to an N-wise `map` (embed each chunk) — and each element caches
    /// separately.
    #[serde(default)]
    pub for_each: Option<String>,
    /// JSON-schema structured output for a `model:` step: the model is
    /// constrained to emit conforming JSON. A general primitive — any extraction
    /// step (e.g. enrichment atoms) declares its output *shape* as data here,
    /// rather than parsing free text. Authored as a TOML table.
    #[serde(default)]
    pub structured_output: Option<toml::Value>,
    /// Lark grammar constraining a `model:` step's output — the lower-level
    /// alternative to `structured_output` when a JSON schema isn't expressive
    /// enough. Templated like `prompt`.
    #[serde(default)]
    pub grammar: Option<String>,
    /// A templated JSON object merged into this step's output object — the
    /// workflow analog of "the runner stamps `chapter_id` from the dispatched
    /// input." Resolved against the same scope as the rest of the step (so under
    /// `for_each` it sees `{element.*}`), then merged into the (object) output,
    /// overriding any same-named key. This is the general way to carry
    /// per-element identity through a `for_each` map: e.g.
    /// `stamp = { chapter_id = "{element.index}" }` turns a model's
    /// `{questions}` into `{chapter_id, questions}` keyed per chapter — a
    /// `Vec<ExtractedQuestion>`-shaped collection, not an anonymous array. A
    /// non-object output (text, a collection) leaves the stamp inert (warned).
    #[serde(default)]
    pub stamp: Option<toml::Value>,
    /// For a `model:` step with `structured_output`/`grammar`: keep the grammar
    /// constraint but return the **raw** model text instead of auto-parsing it to
    /// a `Json` artifact. Needed when a downstream step does the parsing itself —
    /// e.g. `pipeline_parse` running the bespoke `parse_<phase>` over the exact
    /// response (its scrub/validate/derive operates on the raw form).
    #[serde(default)]
    pub raw_output: Option<bool>,
    /// Per-element error policy for a `for_each` step. `"skip"` records the
    /// failing element in the step's `failures` and continues with the rest —
    /// the real Phase-1 skip-and-continue, so one bad chapter doesn't abort a
    /// whole-book enrich. `"abort"` (the default) fails the item on the first
    /// element error. Ignored for non-`for_each` steps.
    #[serde(default)]
    pub on_error: Option<String>,
}

impl StepSpec {
    /// Concatenation of every template string this step carries — scanned for
    /// `{ref.key}` to derive edges.
    fn templated_text(&self) -> String {
        let mut s = String::new();
        for f in [
            &self.prompt,
            &self.system,
            &self.system_file,
            &self.input,
            &self.grammar,
        ] {
            if let Some(t) = f {
                s.push(' ');
                s.push_str(t);
            }
        }
        for v in [&self.params, &self.structured_output, &self.stamp] {
            if let Some(p) = v {
                // Debug (not Display) — robust across toml versions; we only need
                // the `{ref.key}` substrings to appear for edge scanning.
                s.push(' ');
                s.push_str(&format!("{p:?}"));
            }
        }
        s
    }
}

/// Where the per-item driver gets its items.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Source {
    Folder {
        path: String,
        #[serde(default)]
        glob: Option<String>,
    },
    List {
        path: String,
    },
    Inline {
        items: Vec<String>,
    },
}

/// One enumerated item: its `{item.*}` fields plus a content `fingerprint`
/// (mtime+size when the value is a file) folded into every step's cache key, so
/// editing a source file invalidates that item's cached steps.
#[derive(Debug, Clone)]
pub struct SourceItem {
    pub fields: BTreeMap<String, String>,
    pub fingerprint: String,
}

impl Source {
    /// Enumerate items, each exposing `{item.path/name/stem}` + a fingerprint.
    /// `params` resolves `{param.*}` in the source's paths/globs/items, so a
    /// workflow takes its folder (etc.) at run time (`--folder`/`--param`).
    pub fn enumerate(&self, params: &Arc<BTreeMap<String, String>>) -> Result<Vec<SourceItem>> {
        // A param-only scope: `{item.*}`/`{element.*}` don't exist at enumeration
        // time, but `{param.*}` does.
        let pscope = Scope {
            params: Arc::clone(params),
            ..Default::default()
        };
        let resolve = |s: &str| crate::template::resolve_str(s, &pscope);
        match self {
            Source::Inline { items } => {
                Ok(items.iter().map(|v| make_item(&resolve(v))).collect())
            }
            Source::List { path } => {
                let path = resolve(path);
                let text = std::fs::read_to_string(&path)
                    .map_err(|e| Error::Execution(format!("workflow source list {path}: {e}")))?;
                Ok(text
                    .lines()
                    .map(|l| l.trim())
                    .filter(|l| !l.is_empty())
                    .map(make_item)
                    .collect())
            }
            Source::Folder { path, glob } => {
                let path = resolve(path);
                let glob = glob.as_ref().map(|g| resolve(g));
                // Glob: a comma-separated list of `*.<ext>` patterns (e.g.
                // `*.pdf,*.txt,*.md`) filters by extension, case-insensitively —
                // so a mixed folder ingests in one pass. A glob that resolves to
                // empty (an unset `{param.glob}`) or carries no `*.` pattern
                // matches every file.
                let exts: Vec<String> = glob
                    .as_deref()
                    .map(|g| {
                        g.split(',')
                            .filter_map(|p| p.trim().strip_prefix("*.").map(|e| e.to_string()))
                            .collect()
                    })
                    .unwrap_or_default();
                let dir = std::fs::read_dir(&path)
                    .map_err(|e| Error::Execution(format!("workflow source folder {path}: {e}")))?;
                let mut out = Vec::new();
                for entry in dir.flatten() {
                    let p = entry.path();
                    if !p.is_file() {
                        continue;
                    }
                    if !exts.is_empty() {
                        let matches = p
                            .extension()
                            .and_then(|e| e.to_str())
                            .map(|e| exts.iter().any(|x| x.eq_ignore_ascii_case(e)))
                            .unwrap_or(false);
                        if !matches {
                            continue;
                        }
                    }
                    let mut item = make_item(&p.to_string_lossy());
                    // Surface the file's text as `{item.text}` so the common
                    // "a model over each file" workflow needs no separate read
                    // step. UTF-8 only and size-capped: a binary (PDF, audio,
                    // image) or an oversized file simply has no `text` field and
                    // flows through a tool/MCP step instead. The mtime:size
                    // fingerprint already keys the cache, so an edit to the file
                    // still invalidates that item's cached steps.
                    const MAX_ITEM_TEXT: u64 = 1 << 20; // 1 MiB
                    let small = p.metadata().map(|m| m.len() <= MAX_ITEM_TEXT).unwrap_or(false);
                    if small {
                        if let Ok(text) = std::fs::read_to_string(&p) {
                            item.fields.insert("text".into(), text);
                        }
                    }
                    out.push(item);
                }
                out.sort_by(|a, b| a.fields.get("path").cmp(&b.fields.get("path")));
                Ok(out)
            }
        }
    }
}

fn make_item(value: &str) -> SourceItem {
    SourceItem {
        fields: item_fields(value),
        fingerprint: fingerprint_for(value),
    }
}

fn item_fields(value: &str) -> BTreeMap<String, String> {
    let p = Path::new(value);
    let name = p
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(value)
        .to_string();
    let stem = p
        .file_stem()
        .and_then(|n| n.to_str())
        .unwrap_or(&name)
        .to_string();
    let mut m = BTreeMap::new();
    m.insert("path".into(), value.to_string());
    m.insert("name".into(), name);
    m.insert("stem".into(), stem);
    m
}

/// A cheap content fingerprint: `mtime:size` when `value` is an existing file
/// (so an edit invalidates the cache), else the value itself.
fn fingerprint_for(value: &str) -> String {
    if let Ok(md) = std::fs::metadata(value) {
        if md.is_file() {
            let mtime = md
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs())
                .unwrap_or(0);
            return format!("{mtime}:{}", md.len());
        }
    }
    value.to_string()
}

/// A parsed workflow.
#[derive(Debug, Clone)]
pub struct Workflow {
    pub name: String,
    pub source: Option<Source>,
    pub steps: Vec<StepSpec>,
}

#[derive(Deserialize)]
struct WorkflowFile {
    workflow: WorkflowMeta,
    #[serde(default)]
    source: Option<Source>,
    #[serde(default, rename = "step")]
    steps: Vec<StepSpec>,
}

#[derive(Deserialize)]
struct WorkflowMeta {
    name: String,
}

impl Workflow {
    pub fn parse(toml_str: &str) -> Result<Self> {
        let f: WorkflowFile = toml::from_str(toml_str)
            .map_err(|e| Error::Execution(format!("workflow parse: {e}")))?;
        if f.steps.is_empty() {
            return Err(Error::Execution("workflow has no [[step]] entries".into()));
        }
        let mut seen = HashSet::new();
        for s in &f.steps {
            if !seen.insert(s.id.as_str()) {
                return Err(Error::Execution(format!("duplicate step id `{}`", s.id)));
            }
        }
        Ok(Workflow {
            name: f.workflow.name,
            source: f.source,
            steps: f.steps,
        })
    }

    /// DAG edges `(from, to)`: a step referencing step `r`'s output depends on `r`.
    pub fn edges(&self) -> Vec<(usize, usize)> {
        let index: BTreeMap<&str, usize> = self
            .steps
            .iter()
            .enumerate()
            .map(|(i, s)| (s.id.as_str(), i))
            .collect();
        let mut edges = Vec::new();
        for (i, s) in self.steps.iter().enumerate() {
            for r in template::referenced_ids(&s.templated_text()) {
                if let Some(&j) = index.get(r.as_str()) {
                    if j != i {
                        edges.push((j, i));
                    }
                }
            }
            // A `for_each` step depends on the collection it maps over.
            if let Some(fe) = &s.for_each {
                if let Some(&j) = index.get(fe.as_str()) {
                    if j != i {
                        edges.push((j, i));
                    }
                }
            }
        }
        edges
    }

    /// Topological order (ascending-index within a level for determinism);
    /// errors loudly on a cycle.
    pub fn topo_order(&self) -> Result<Vec<usize>> {
        let n = self.steps.len();
        let mut indeg = vec![0usize; n];
        let mut adj: Vec<Vec<usize>> = vec![vec![]; n];
        for (from, to) in self.edges() {
            adj[from].push(to);
            indeg[to] += 1;
        }
        let mut heap: BinaryHeap<Reverse<usize>> =
            (0..n).filter(|&i| indeg[i] == 0).map(Reverse).collect();
        let mut order = Vec::with_capacity(n);
        while let Some(Reverse(i)) = heap.pop() {
            order.push(i);
            for &j in &adj[i] {
                indeg[j] -= 1;
                if indeg[j] == 0 {
                    heap.push(Reverse(j));
                }
            }
        }
        if order.len() != n {
            let stuck: Vec<&str> = (0..n)
                .filter(|i| !order.contains(i))
                .map(|i| self.steps[i].id.as_str())
                .collect();
            return Err(Error::Execution(format!(
                "workflow has a cycle among steps: {stuck:?}"
            )));
        }
        Ok(order)
    }
}
