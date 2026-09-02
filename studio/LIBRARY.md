# The library realization of the workflow substrate

Status: exploration, 2026-09-02. Nothing here is built. `BOUNDARY.md` says
what the studio package may depend on; this document asks what it would take
to lift the engine underneath it out of the monorepo as a library that ships
on crates.io, PyPI, npm and as a Go module, that commonwealth-ai then consumes
back as an ordinary dependency, and what the one-sentence argument for that
library is.

## What exists, measured

Line counts are `wc -l` over `src/**/*.rs` on this branch; code intel was down
when they were taken, so the citations are grep, not `symbols`.

| Crate | Lines | What it is |
|---|---|---|
| `sovereign-workflow` | 3,515 | The engine. Seven modules: `model` (TOML → `Workflow`), `kind` (the six step kinds), `steps`, `runner`, `template`, `cache`, `progress`. |
| `sovereign-tools-base` | 7,982 | Leaf tools: file/json/csv/zip read-write, shell, web fetch and search, chunk, section, vector mean, and a full MCP client (stdio and HTTP transports, auth, reconnect, secret store). |
| `sovereign-workflow-host` | 2,170 | Registry assembly, the in-process runner entry, seven shipped workflows, the `recipe:` installer, the NL workflow-author tools. |
| `sovereign-recipe-author` | 6,679 | Recipe authoring and its SQLite project store. |
| `sovereign-studio` | 517 | The headless CLI proving the package runs against any OICP host. |
| `sovereign-contracts` (shared leaf) | 24,619 | Sovereign's whole domain vocabulary. |
| `oicp-client` (shared leaf) | 2,904 | HTTP client for the manifest and the OpenAI-compatible surface. |
| `oicp-types` (shared leaf) | 4,933 | The wire types. |

The engine is small and its authoring surface is smaller. A workflow is a TOML
file with three tables: `[workflow]`, an optional `[source]` (folder, glob,
inline list), and `[[step]]` entries. A step has an `id`, a `uses` of one of
six kinds (`model:`, `embed:`, `tool:`, `mcp:`, `transform:`, `recipe:`), and
templated arguments that reference `{step.key}`, `{item.field}`,
`{element.field}` or `{param.key}`. Edges are derived from the references, not
declared. `for_each` maps a step over a JSON array. Read-effect steps are
content-addressed by their resolved inputs plus the source file's fingerprint
and served from `~/.svrnmesh/workflow-cache` on re-run; write-effect steps are
never cached. `model:` steps take `structured_output` or a Lark grammar and
yield a parsed JSON artifact. Every step emits one tracing event and one
progress event. `docs/WRITE_A_WORKFLOW.md` is the whole user manual, and the
seven shipped files under `sovereign-workflow-host/recipes/` are each under
forty lines.

The lift itself has already been done once. `~/dev/svrn-workflow-sandbox`
(2026-07-21, local, uncommitted) copied the eight-crate closure out of the
monorepo with zero source edits, built in 36 seconds cold, and ran `summarize`
against a live daemon with the cache working on re-run. It surfaced the one
structural snag: `sovereign-contracts` carries an `include_str!` that escapes
its crate root to reach `sovereign-recipes/`, so the sandbox had to preserve
the monorepo's directory shape to compile. That sandbox is a whole-package
lift with the contract crate carried along. What this document proposes is the
same lift with the contract cut, which is what turns a sandbox into a
dependency.

Four monorepo binaries already consume the engine as a library: `sovereign-cli-llm`,
`sovereign-server`, `sovereign-cli-daemon` and `sovereign-desktop`. The
substrate spec (`sovereign/docs/specs/WORKFLOW_SUBSTRATE.md`) proved it
reproduces corpus ingest's `chunk → embed` stage byte-for-byte against the real
engine. So "consume within it" is not a future state. It is the current state,
and the library question is only about the dependency direction of the
contract.

## The one thing it does well

Every existing LLM workflow product is a framework: LangGraph, DSPy, Haystack,
Mastra, the OpenAI Agents SDK. Each owns the program. Your pipeline is code in
their language, against their abstractions, and moving it to another language
means rewriting it. The visual ones (Dify, n8n, Flowise) own the program the
other way, as a proprietary graph inside a hosted editor.

The substrate's distinguishing property is that the workflow is a file. Not a
DSL embedded in a host language, and not an export format: the file is the
program, and the engine is an interpreter for it. That is the `make` position.
`make` does not care what language your build is in, it runs any host with a
shell, and its one file format outlived every build framework that tried to own
the build. The argument, in one sentence:

> Run a declarative LLM workflow file against any OpenAI-compatible host, with
> content-addressed caching, from any language, and do nothing else.

The "nothing else" is the load-bearing half, and it should be written down as
a list of refusals, because each of these is something the frameworks do and
each is somewhere the substrate would lose its shape:

- No agent loop. A step is one call; a loop is a `for_each` over data the
  author can see.
- No memory, no vector store, no retrieval. `embed:` produces a vector; what
  you do with it is a tool.
- No prompt library or prompt optimizer. A prompt is a string in the file, or a
  file the step names.
- No hosted tracing, no dashboard. Progress is a stream of events on stdout or a
  callback; tracing goes wherever the host's tracing goes.
- No model routing beyond "this URL, this model name". The mesh's latency
  classes and slot policy are Sovereign's business.

What it keeps, and what nothing else in the field offers as a unit: the
derived DAG, the fingerprint cache, structured output as a first-class artifact,
`for_each` with bounded concurrency, and MCP as the tool protocol on both sides
(consumed by `mcp:` steps, and, see below, the way a host language contributes
a tool).

The sandbox's distribution decision points the same way. The operator locked
a "grey market" model on 2026-07-21: the core team curates the shipped
workflows as a standard library, and the community distributes its own TOML
files off-repo, with provenance labels and a specific consent card as the
safety model. That model only works if the file is the program. A library
that runs the same file from four languages is the grey market's runtime.

The four languages fall out of the file being the program. A Python user and a
Go user run the same `summarize.toml`. The packages are hosts for one
interpreter, not four implementations of one idea, and that is the property
the polyglot frameworks structurally cannot have.

## The cut

Three things stand between the engine as it sits today and a liftable library.
None is large; one is a decision rather than work.

**The contract crate is the anchor.** `sovereign-workflow` declares itself
"contract-only by design" and depends on `sovereign-contracts`, which is
24,619 lines and carries recipes, setup config, MCP config, skills, intent
policy, the run lock and the rest of Sovereign's vocabulary. The engine
actually names about a dozen items from it: `Error`/`Result`, `StepOutput`,
`Effect`, `ToolContext`, `CompletionRequest`, `Speed`, `LatencyClass`,
`InferenceRequirements`, `ToolRegistry`, `InferenceProvider`,
`CorpusInstaller`/`InstallOutcome`, and `latency_to_speed` (grep of
`sovereign_contracts::` under `sovereign-workflow/src`). `tools-base` uses
the tool half of the same set plus `Permission` and `ToolDescriptor`. The
library needs a contract crate of a few hundred lines holding exactly those
shapes, and `sovereign-contracts` re-exports them at its current paths so no
importer in the monorepo changes. This is the B:P1 carve-out pattern run once
more, one layer further down, and the boundary gate already has a table to add
the new leaf to. It is the single piece of work that makes the rest possible,
and it is where "one decider, one name" lives: the monorepo must consume the
published types, not a copy. It also retires the escaping `include_str!` the
sandbox tripped on, since the recipe schema has no business in a leaf the
engine depends on.

**Two step kinds and one tool are Sovereign-shaped.** `recipe:` delegates to a
corpus install over the daemon's internal port through the `CorpusInstaller`
trait; in the library that trait is an extension point with no default
implementation, and the host crate keeps the HTTP one. `model:thoughtful`,
`model:fast` and the `embed:default` alias are OICP latency classes; against a
plain OpenAI-compatible host the library needs a model map in the run config
(`thoughtful = "gpt-…"`), with the manifest-driven resolution staying in
`oicp-client` as the Sovereign host's implementation of the same seam. And
`tool:extract`, which the flagship `summarize.toml` uses on its first line, is
monolith-side (it is one of the five injected via `extra_tools`). The library
either ships a plain-text/Markdown/HTML extract leaf and documents that PDF
and Office need the Sovereign host, or the flagship example changes. Naming
the gap is better than shipping an example that does not run.

**The licence is a decision, not a task.** The monorepo is AGPL-3.0-or-later
by a single workspace declaration. That is the right licence for the product
and the wrong one for a library: a package on PyPI or npm that pulls its
linker into AGPL is one nobody adds to a product, and the ecosystem-integration
notes already record the licence split as a lever. The lifted crates would need
their own permissive licence (Apache-2.0 or MIT), which is the operator's call
and must precede the first publish, since a licence cannot be quietly changed
after. The same goes for the name: a library called `sovereign-anything`
carries the product's brand into someone else's dependency tree, and the
studio's own `sovereign-` prefix would not survive the lift.

What stays behind is everything that is a product feature rather than an
interpreter: `sovereign-recipe-author` whole, the workflow-author tools (the
NL authoring loop runs in `sovereign-server`'s runtime and needs a model with
a conversation), the corpus and atlas tools, the desktop surfaces, and the
`recipe validate`/`recipe test` verbs of `sovereign-studio`. The studio package
stays exactly as it is and becomes the first consumer of the library.

## Four packages from one interpreter

The honest way to ship four languages is not four bindings. It is one Rust
core with one process boundary, and the four packages differ only in how thick
that boundary is.

The core exposes three verbs and the packages expose the same three:

1. `parse` a workflow file and report its steps, its derived edges, and the
   `{param.*}` keys it needs filled. This is what the author tools already call
   `validate`.
2. `run` it with a host (URL, API key, model map), params, a concurrency bound,
   and a cache directory, returning the per-item report the runner already
   produces.
3. `observe` progress: the existing `StepObserver` events as a stream.

Rust is the crate itself. Python and Node are the two languages that matter for
adoption, and both have a mature native path (`pyo3`/`maturin`, `napi-rs`) that
ships a wheel or a prebuilt `.node` per platform, keeps the runner in-process,
and gives the host language a real async handle. Go has no pleasant path to a
Rust library: cgo is a build-tooling tax on every user. For Go the package is a
thin client for the CLI binary running as a sidecar over stdio with a JSON
protocol, which is the language-server pattern and costs nothing the core
does not already have (the progress stream is already a sequence of events;
the report is already a struct).

That sidecar protocol is worth building first even for Python and Node,
because it is the cheapest route to four packages of a few hundred lines each,
and the native bindings can replace it later without changing the package's
surface. It is also the only route that keeps the four packages honest: if the
protocol is the contract, a Python user and a Go user see the same events and
the same errors, which the "one file, any language" argument requires.

The one problem across any boundary is a tool implemented in the host
language: a `tool:` step whose body is a Python function. The tempting design
is a reverse channel from core to host. The existing design already has one.
The engine speaks MCP as a client over stdio, so a host-language tool is an
MCP server the SDK starts in-process, and the workflow names it as an `mcp:`
step. The SDK's contribution is a twenty-line decorator that turns a function
into that server. No new protocol, no FFI callbacks, and the tool works
identically from the CLI, from the desktop and from a Go program. This is the
kind of reuse the inventory principle exists to catch, and it should decide the
design.

## What consuming it back costs the monorepo

Less than it looks, because the monorepo already consumes the engine through a
path dependency and a boundary gate. The changes are:

- The workspace's `sovereign-workflow`, `sovereign-tools-base` and the new
  contract leaf become path dependencies on a subtree that is also published,
  with a version. Cargo handles a crate being both a path dep and a crates.io
  crate; the version bump is the only new ceremony.
- The workflow file format gets a declared version key, and semver applies to
  the file format, not just the crate API. The file is the public interface;
  the crate is an implementation detail of it. A shipped workflow that parses
  under 1.x must parse under 1.y.
- The boundary gate's leaf table gains the new contract crate and loses the
  engine's edge to `sovereign-contracts`. The gate is already the instrument
  that would catch a regression, and it already runs blocking in CI.
- The `chunk → embed` byte-diff proof becomes the lift's acceptance test: the
  same workflow, run once through the monorepo binary and once through the
  lifted CLI against the same daemon, must produce the same artifacts. That is
  a test that already exists in shape, pointed at a second binary.

## Sequence, if it goes ahead

Each of these is a session, and the first is not engineering.

1. Name and licence, decided by the operator. Nothing publishes before this.
2. The contract leaf: carve the dozen shapes out of `sovereign-contracts` into
   the new crate, re-export at the old paths, widen the boundary table. Gate:
   the workspace builds with zero importer churn and the boundary gate is green.
3. The engine and `tools-base` depend only on the leaf. `recipe:` becomes an
   extension trait; the latency-class resolution becomes a host seam with the
   OICP implementation in `oicp-client`. Gate: `sovereign-studio` still runs the
   notebook workflow end to end against the daemon.
4. The CLI with a `--json` protocol mode: parse, run, observe, over stdio.
   Gate: the byte-diff proof against the monorepo binary.
5. The Python package over the sidecar, with the MCP tool decorator. Gate:
   `summarize.toml` (with the extract caveat resolved one way or the other)
   runs from a ten-line Python script against a plain OpenAI-compatible server.
6. Node and Go over the same protocol. Native Python and Node bindings only if
   the sidecar's process cost shows up for a real user.

The failure this sequence is designed against is the obvious one: four
packages that drift into four products. The protocol in step 4 is the fence.
Everything a package adds beyond the three verbs and the tool decorator is a
smell, in the same sense `BOUNDARY.md` treats a dependency on `sovereign-core`
as one.
