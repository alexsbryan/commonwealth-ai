# Commonwealth AI — System Overview

A navigation primer. Read this on day one to know what exists, how
the pieces fit, and where to look for each subsystem. Read
[`sovereign/ARCH_PRINCIPLES.md`](./ARCH_PRINCIPLES.md) on day two for
the rules of engagement. (Brand new? Start with the ten-minute
[`docs/ARCHITECTURE_TOUR.md`](../docs/ARCHITECTURE_TOUR.md) — a
compressed rendering of this contract for newcomers. It summarizes;
this file is the truth.)

This file is a contract per `ARCH_PRINCIPLES.md §1.1`: every claim
must be verifiable against the code on the commit it appears in. If
you change a subsystem, update its entry in the same PR. It states
what IS; how the system came to be this shape — the reversals,
decompositions, and archaeology — lives in
[`HISTORY.md`](./HISTORY.md), linked from the entries it explains.
Capabilities that exist but ship default-off or dark are tracked in
[`DEFAULTS_LEDGER.md`](./DEFAULTS_LEDGER.md) — each with the
falsifiable condition that flips it and a review-by date; shipping
dark without a ledger row is a contract violation.

---

## 1. The projects

```
commonwealth-ai/
├── oicp-types/                # OICP wire types — no other deps
├── oicp-client/               # OICP pure-HTTP client (OpenAI-compat + manifest routing)
├── corpus-engine/             # Knowledge layer (LanceDB + Tantivy)
├── corpus-engine-scip/        # SCIP call graph + per-language exporter dispatch
├── corpus-engine-notes/       # NoteStore + project_docs index (carved out of corpus-engine)
├── corpus-engine-atos/        # ATOS feature store + plan items + design signals (carved out) — opt-in behind `--features atos`
├── corpus-engine-archaeology/ # Git archaeology + rough-edges + atom-provenance (carved out)
├── corpus-engine-yield/       # YieldHook cooperative-yield contract (Tier-0 leaf shared by the data plane + watchers)
├── corpus-engine-watchers/    # Lint/test/project-index watchers + result stores (carved out of corpus-engine)
├── sovereign-recipes/         # Canonical recipe TOMLs + catalog + data lists (vendored into corpus-engine at build)
├── sovereign/                 # Local AI assistant (CLI / desktop / server)
├── commonwealth/              # Mesh coordination daemon
├── studio/                    # Liftable authoring package — workflow engine + recipe-author + headless CLI (see studio/BOUNDARY.md)
├── quality/                   # Quality program — ARCH_LAYERS.toml (layer map), gate baselines, arch-layers crate
├── packages/chat-ui/          # Shared Svelte chat render surface (desktop + mobile)
├── packages/vscode-sovereign/ # First-party VSCode FIM extension (ghost text; zero-dep esbuild bundle)
└── sovereign-mobile/          # Thin Tauri 2 mobile client (iOS + Android), tailnet or iroh dial-by-key
```

Supporting directories outside the project tree: `vendor/` (pinned
`llama-cpp-4`), `scripts/` (CI bench + corpus setup), `docs/`,
`landing/`, `gym/`, `baselines/`, and `corpus-engine/xtask` (the
docs-gate / arch-gate CI binaries). `models/` holds downloaded GGUF
weights (created by `svrn setup`, gitignored).

| Project              | Role                                          | Depends on                                            |
|----------------------|-----------------------------------------------|-------------------------------------------------------|
| `oicp-types`         | OICP v0.3 wire types + scoring helpers        | —                                                     |
| `corpus-engine`      | Acquire → extract → filter → chunk → embed → index | `oicp-types`, `corpus-engine-yield`, `corpus-engine-scip` (treesitter feature), `corpus-engine-notes`, `corpus-engine-atos` |
| `corpus-engine-scip` | SCIP call graph store + exporter dispatch     | —                                                     |
| `corpus-engine-notes`| NoteStore + project-docs index + notes↔alignment sync (carved out of corpus-engine for blast-radius control) | `rusqlite` |
| `corpus-engine-atos` | ATOS feature store + plan items + DESIGN.md design signals (carved out). **ATOS is an opt-in experiment** behind the `atos` Cargo feature — the recipe-author workspace uses `sovereign-store::RecipeProjectStore` instead, and default product builds (server/desktop/daemon/cli) carry zero ATOS | `rusqlite` |
| `corpus-engine-archaeology` | Git history mining + rough-edge surfacing + atom-provenance eval (carved out) | — |
| `corpus-engine-yield` | `YieldHook` cooperative foreground-yield contract — a Tier-0 leaf (one trait, zero deps) shared by the data plane and the watchers so the daemon's `Arc<dyn YieldHook>` has one trait identity on both | — |
| `corpus-engine-watchers` | Lint/test/project-index watchers + their SQLite result stores + coordinator (carved out of corpus-engine, R4 Step 1 — cuts the watcher-edit rebuild set 22→12 crates, measured). Compiles unconditionally; the SCIP `CodeWatcher` stays in corpus-engine | `corpus-engine-notes`, `corpus-engine-yield`, `rusqlite`, `notify` |
| `sovereign-recipes`  | Canonical recipe TOMLs + catalog + data lists (vendored into corpus-engine at build) | —                                       |
| `sovereign`          | Local agent runtime                           | `corpus-engine`, `corpus-engine-scip`, `oicp-types`   |
| `commonwealth`       | Symmetric mesh daemon                         | `corpus-engine`, `oicp-types`                         |

Dep direction is one-way. Sovereign optionally embeds cmnwlth
in-process via `sovereign-mesh` — the only place the two upper
projects meet.

```
       oicp-types          sovereign-recipes
            │              (recipe TOMLs + data/)
            │                       │ build.rs include_bytes!
            │                ┌──────▼──────┐
            │                │ corpus-engine│  (LanceDB + Tantivy)
            │                └──────┬──────┘
            │                       │  EmbedFn / InferenceFn
            ├───────────┬───────────┼──────────────┐
            │           │           │              │
        Sovereign       │      both call          cmnwlth
       (sovereign/)     │   identical APIs        (commonwealth/)
            │           │                              │
            └─ sovereign-mesh (in-process embed) ──────┘
```

Two shared protocols cross the Sovereign/cmnwlth boundary:

- **OICP** — declared in `commonwealth/docs/oicp-v0.3.md`; types
  in `oicp-types/src/lib.rs`; re-exported as `sovereign_core::oicp`
  and `commonwealth_core::oicp`. Downstream crates use the
  re-exports, never the types crate directly.
- **`EmbedFn` / `InferenceFn`** — `corpus-engine` accepts these
  closures from any caller; each project supplies its own
  implementation.

---

## 2. Workspace map

One line per crate. For folder-level detail, `ls` the crate's `src/`
or read its `lib.rs`. See `sovereign/docs/` for subsystem deep
dives.

### corpus-engine

A self-contained library between "raw source on the internet" and
"ranked search hits with provenance." See
[`corpus-engine/README.md`](../corpus-engine/README.md),
[`ENRICHMENT_V2.md`](../corpus-engine/ENRICHMENT_V2.md),
[`ATLAS.md`](../corpus-engine/ATLAS.md), and
[`INCREMENTAL_ATLAS.md`](../corpus-engine/INCREMENTAL_ATLAS.md).

Major modules under `corpus-engine/src/`:

- `engine/` — `CorpusEngine` façade (`ingest`, `expand`, `reindex`)
- `acquirers/`, `extractors/`, `chunkers/`, `filters/` — pipeline stages
- `asset_store/` — content-addressed filesystem store for binary
  payloads (raw bytes + optional typed parsed-form caches +
  append-only ledger). Architecture-over-Enron AD-1; the substrate
  the described-asset dispatcher and future calendar /
  transactions / sensor verticals share.
- `recipe.rs`, `registry.rs` — TOML schema + recipe catalog
- `index/` — LanceDB (IVF-PQ) + Tantivy FTS, `IndexMeta`, `ScopeMeta`
- `enrichment/` — v1 field-engine (5-phase domain pipeline) and v2
  atlas (typed atom graph; `Pipeline` trait + registry + exemplar
  bank). See `ENRICHMENT_V2.md`. Plus `enrichment/reconciliation/`
  — the multi-origin merge primitive (Phase 4 of the architecture-
  over-Enron push) with reversible oplog + pluggable merge signals.
  Signals are **identity-grade only** (exact name fold, nickname /
  initial-surname, exact shared email or email-alias, org+role) —
  the fuzzy email↔name and bare-name-alias paths were removed after
  they chained thousands of atoms into one polluted cluster.
  `candidate_pairs` blocking keeps the O(n²) scan sub-second;
  corporate-suffix normalization (`strip_org_suffixes`,
  Institution-only) folds "El Paso Corp."-style variants.
  `svrn bench enron diagnose` is the glass-box.
- `atlas_traversal/` — query layer over atlas graphs
- `update/` — code/file watchers, delta updates, lint/test watchers
- `meta_atlas/` — cross-corpus articulation classifier + index
- `pii.rs`, `alignment_projector.rs` — operator-facing scanners
- **Carved out into sibling crates** (see §1): NoteStore +
  project-docs index → `corpus-engine-notes` (`notes.rs`,
  `project_docs.rs`); ATOS FeatureStore + plan items + design
  signals → `corpus-engine-atos` (`features.rs`, `plan_items.rs`,
  `design_signals.rs`); git archaeology + rough-edges + provenance
  eval → `corpus-engine-archaeology` (`git_archaeology.rs`,
  `rough_edges.rs`, `archaeology_eval.rs`)

### sovereign

```
crates/
├── sovereign-core           # Traits, runtime, planner, executor, router, memory
├── sovereign-inference      # llama.cpp slots, remote OpenAI-compat, hybrid w/ failover
├── sovereign-store          # SQLite + Postgres + in-memory StateStore
├── sovereign-tools          # Built-in tools (search, knowledge, docs, web, MCP, code-intel)
├── sovereign-gliner         # GLiNER (ONNX) per-chunk entity extraction — own crate to keep the ONNX dep off the shared sovereign-tools. Two backends (v1 gline-rs, GLiNER2 bare-ort) behind `LabeledEntityExtractor`; `load_labeled_extractor` is the one selector
├── sovereign-atos           # ATOS lib (charter, approval, report, session, local orchestrator) — opt-in experiment behind `--features atos`; no product crate depends on it by default
├── sovereign-work-atlas     # Coordination atlas for agents on the mesh
├── sovereign-mesh           # In-process cmnwlth embed
├── sovereign-compute        # Supervised compute-child process boundary (P1): child-process supervisor + native lossless wire + child server/entrypoint + daemon-side single-child routing facade. Value = crash isolation + distributed case, NOT parallelism (see doc)
├── sovereign-server         # Axum REST + WebSocket, multi-tenant + approvals
├── sovereign-desktop        # Tauri 2 + Svelte 5
├── sovereign-cli            # User-facing dispatcher — execs into sibling binaries
├── sovereign-cli-shared     # Tiny shared lib (dirs, repo, help, prompts, tracing init)
├── sovereign-time           # Wall-clock helpers (Unix-epoch secs/millis) — a zero-dep leaf for crates that don't depend on sovereign-core
├── sovereign-cli-daemon     # Long-running host + lifecycle (~241 MB binary; bin+lib — the desktop's --daemon-child re-enters via daemon_child_main)
├── sovereign-cli-dev        # Workbench: ATOS + project lifecycle + code intel + tools
├── sovereign-cli-llm        # Model interaction + heavy retrieval (chat/bench/eval/atlas/…)
├── sovereign-pipeline       # Pipeline / pod-lifecycle helpers
├── sovereign-eval           # Eval surfaces
├── sovereign-authoring-harness # Recipe-authoring verdict ladder — Pass/Fail policy + render over corpus-engine harness StageOutputs
├── sovereign-meshapp        # Mesh-app explorer ops — pure path-in/DTO-out lib shared by the desktop bridge + `meshapp dev` (§5)
├── sovereign-agent-bench    # Eleven-problem agent-coding battery
├── commonwealth-agent-tools # Canonical agent-tool primitives (cross-runner contract)
└── commonwealth-tdd         # Unified TDD solver loop (HTTP + MCP transports)
```

Top-level: `modes/` (skills — recipe-author, inner-work),
`models.toml`, `models/`, `bench/`, `inquiries/`, `router/`,
`sovereign-server.toml`.

### commonwealth

```
crates/
├── commonwealth-core         # Shared types — ids, mesh, capabilities, ledger, aliases
├── commonwealth-transport    # PeerTransport seam — (peer, traffic class) → endpoints; IP today, iroh-ready
├── commonwealth-discovery    # mDNS, gossip, latency probe, hardware, TLS, peering
├── commonwealth-inference    # Scheduling + orchestration
├── commonwealth-api          # HTTP servers (client 9741 + internal 9742, plaintext)
├── commonwealth-knowledge    # corpus-engine integration over the mesh
├── commonwealth-app          # Mesh-app platform (manifest, lifecycle, proxy)
├── commonwealth-state        # MeshStore — gossip-replicated SQLite KV w/ TTL GC
├── commonwealth-daemon       # CLI entry + signal handling
└── commonwealth-test-harness # SimulatedMesh, SimulatedNode, MockLlamaServer
```

`contrib/` ships `install.sh`, systemd unit, launchd plist.
`docs/oicp-v0.3.md` is the canonical OICP spec.
`commonwealth/crates/oicp-conformance` is the standalone OICP v0.4
host conformance tester — minimal deps (oicp-types + HTTP), liftable
by any third party certifying their own implementation.

### studio

The liftable authoring package (`studio/crates/`) — buildable against
only the OICP contract crates, enforced by the xtask `boundary-gate`
(contract: `studio/BOUNDARY.md`).

```
crates/
├── sovereign-workflow       # Step·Artifact·Runner — typed dataflow over local-model steps (P0+P1 + content cache + `for_each` collection-map; `svrn workflow run`). Diffed byte-for-byte against the real corpus chunk→embed stage. Owns the `StepKind`/`WireKind` wire-kind catalog the authoring schema derives from (§2.1 source of truth).
├── sovereign-workflow-host  # Daemon-runnable workflow host — assembles the standard tool registry + daemon inference + content cache to run a workflow in-process; the catalog/resolve surface; the living trigger; the `recipe:` corpus-ingest stage; and the NL workflow-author tool bundle (`workflow_write`/`_write_structured`/`validate`/`test`, the JSON-Schema-constrained author mirroring recipe-author). Two run entries: `run_workflow_in_process` (builds a daemon-routed provider from a URL — the CLI + living trigger) and `run_workflow_with_provider` (takes an **injected** provider + optional `StepObserver` — the desktop **Run a workflow** view feeds its own `AppState.inference` and streams per-step progress to the UI).
├── sovereign-tools-base     # Pure leaf workflow tools (shell/web/chunk/file/json/csv/zip/vector/MCP) — the tool set the studio package ships without sovereign-tools
├── sovereign-recipe-author  # Recipe-authoring tool bundle + RecipeProject model + project store (re-exported as `sovereign_tools::recipe_author` for legacy paths)
└── sovereign-studio         # Headless studio CLI — authors/tests recipes + runs workflows against any OICP daemon; the proof the package is independently usable
```

### quality

The quality program's policy + baselines (landed 2026-07-11).
`quality/ARCH_LAYERS.toml` is the declared layer map — the
dependency-direction contract enforced by `cargo xtask layer-gate`
(Cargo-declared edges, CI) and the code-intel arch report
(SCIP-observed edges). `quality/arch-layers/` is the tiny shared
schema/evaluator crate both consumers use. `quality/baselines/` holds
the machine-written ratchet baselines (oversized files, fan-in caps,
Cargo.lock duplicates, clippy counts, public-API snapshots) —
regenerated only via `cargo xtask <gate> --update-baseline`, banked
weekly via `--tighten`. `cargo xtask quality` runs every sub-second
local gate with one summary table; `lint-gate` (clippy-count ratchet)
and `api-gate` (public-API surface diffs on the pinned nightly from
`quality/nightly-pin.txt`) run on their own cadence — locally and in
the weekly CI lane, never on the PR critical path.
`quality/CLEANUP.md` is the prioritized cleanup backlog derived from
this instrumentation (arch-report census + lint counts + temporal
coupling), with per-item done-metrics.

### sovereign-recipes

The **single source of truth** for corpus recipes — recipe definitions only.
corpus-engine vendors this tree at build time (`build.rs` → `OUT_DIR`) for the
offline bundle, so there is no second copy. Catalog is `registry.toml`; field
reference is `SCHEMA.md` (generated from `corpus-engine/src/recipe.rs` and gated
by the `recipe_schema` test); `GETTING_STARTED.md` + `_templates/` onboard
contributors.

The catalog (`registry.toml`) lists 26 recipes: `wikipedia`,
`wikipedia-simple`, `wikipedia-newsworthy`, `wikipedia-article`,
`wikipedia-catalog`, `sep`, `stackexchange`, `stackexchange-knowledge`,
`openalex`, `gutenberg`, `gutenberg-work`, `crs_reports`, `us-code`,
`olc-opinions`, `scotus-opinions`, `federal-register-presidential`,
`conversations-anthropic`, `conversations-chatgpt`, the five
`enron-sample*` recipes, and the three `uap-blue-book*` recipes.
Further recipe dirs ship outside the catalog (installed by path or by a
setup script): `codebase`, `arch-principles`, `system-overview`,
`chaos-secret-agent`, `chaos-saltgrass`, `maple-house`, `proxy-company`,
`search-gym`, `sf-assessor-roll`. Underscore directories like
`_templates` carry scaffolding; `meshapp-registry.toml` is the curated
mesh-app registry (§5).

### Bench harnesses

Bench/eval fixtures (`knowledge-gym`, `search-gym`, `routing`,
`book-report`, per-corpus question banks) live under `sovereign/bench/`;
orchestrators in `sovereign-cli-llm/src/bench_cmd/`; pure scorers in
`sovereign-eval/`. The gym *commands* (`svrn search-gym`,
`knowledge-gym`) still exist; only their fixtures moved. Five harnesses
deserve a map entry:

**Model attribution + reliability reports** (cross-cutting). Lane
baselines (`<group>/baselines/<id>/latest.json`, the `LaneBaseline`
schema) historically recorded only the slot alias (`primary`) the run
hit — worthless once the alias is repointed. Capture now resolves the
alias to the **concrete GGUF** at run time
(`bench_cmd/model_resolve.rs` reads the daemon's `/v1/models`
`owned_by: "alias→<stem>"`), stamps the concrete stem into every
transcript row, and records structured `model_attribution`
(`file_stem`/`base_name`/`family`/`quant`, derived in
`sovereign-core::models_manifest::attribution_for_file`). `svrn bench
report` (`bench_cmd/report.rs`) inverts the suite-keyed baselines into a
durable, git-tracked, per-model tree at `sovereign/bench/reports/`
(`index.json` + `<model>/{reliability.json,REPORT.md}`), grouping
quantisations under one model heading but keeping each quant on its own
row, and surfacing any still-unattributed (legacy) baselines rather than
folding them in. The `REPORT.md` renderer is pure + deterministic; the
`reliability.json` is the shape the desktop model-picker card will read.
See `sovereign/bench/reports/README.md`.

**Reasoning-fidelity** (`svrn bench mechanism-fidelity`) — a
*metamorphic* audit of whether a frozen model reasons from a causal
mechanism or a memorized label. A registry of `ReasoningClass`es
(`sovereign-eval/src/mechanism_fidelity/`; three ship:
`wealth_tax_relocation`, `attribution_support`, `aggregation_threshold`)
behind one orchestrator that elicits a forced-choice **logprob**
distribution in one forward pass per probe, keeps a provably-blind
negative control, and runs anytime-valid early-stopping (`stopping.rs`)
to a GO/NO-GO verdict. Each run distils a per-`(model, class)`
**fidelity card** (`~/.sovereign/model-fidelity-cards/<model>.json`,
fingerprint-stamped so stale bands invalidate) — characterize once, read
free per query. Full mechanics:
`sovereign/bench/mechanism_fidelity/README.md`.

**Chaos-Monkey** (`svrn bench chaos-monkey`) — the calibration
counterpart: answer capably + cited **when the facts are in the sealed
corpus**, abstain honestly **when they aren't**, unfooled by
distractors. The bank enforces a fairness contract at load (answerable
items must ship a witness; absent items must not), and a **two-red-line
scorer** never blends competence-when-present with honesty-when-absent,
so neither a hallucinator nor a blanket-abstainer can game it. Drives
the live `handle_message_stream` path sealed to one corpus; the corpus
installs machine-stable from the committed recipe
`sovereign-recipes/chaos-secret-agent/` (`scripts/setup-chaos-corpus.sh`)
so the gate reproduces across boxes. See
`sovereign/bench/chaos_monkey/README.md`.

**Governance** (`svrn bench governance`, FR-9) — gates the
event-sourced common-law tool (the `govern` verbs over a corpus's
`GovernanceView` + `GovernanceOplog`). **Lane A** is a precision/recall
detector over `EdgeType::Tension` edges vs an exhaustive `truth.json`
(scorer: `sovereign-eval/src/governance_bench.rs`). **Lane B** reuses
the chaos two-red-line scorer and adds **RL-3 (no dead law)**: the gated
active-set step in `runtime/retrieval_pipeline.rs` drops retrieved
chunks of amended sections (`GovernanceView::dead_law_sections`) and the
cite-or-abstain gate runs as `GateSurface::Governance`, driving the same
hardened turn `govern ask` ships. The "Maple House" fixture installs
machine-stable from `sovereign-recipes/maple-house/`
(`scripts/setup-governance-corpus.sh`); both lanes gate via
`bench gate governance` / `governance-qa`.

The **desktop Conflicts panel** (`sovereign-desktop`
`governance_commands.rs` + `ConflictsPanel.svelte`) is the "one UX panel"
of the FR-9 thesis: a per-notebook tab (gated on the corpus carrying a
`governance_oplog.jsonl`; `NotebookSummary.open_conflicts` is `Some` only
then) that lists ranked open conflicts with both rule texts, resolves /
accepts / **dismisses** them (dismiss = a new not-a-conflict op, distinct
from accept), and exports the meeting agenda + current-rules sheet. A
folder added via the Library "Rules & decisions" template gets the
generalized governance ontology recipe and, on enrich-build completion,
an automatic **migrate-ids → seed** hook. **Living governance**: the
weekly atlas rebuild renumbers `EdgeId`s, so adjudications also record
their endpoint rule-id pair (`ActiveSet.tension_pairs`) and the view join
matches by edge-id → pair → **mootness** (a conflict whose rule is
superseded is not open), keeping past decisions settled across rebuilds;
only a genuinely dangling decision (a rule's text was edited away)
surfaces as a `GovernanceIssue`.

**Inner-work chaos** (`svrn eval inner-chaos`) — the safety
counterpart of the desktop knowledge chaos loop, re-pointed at the
inner-work witness (the Reflect rail). Per thread it samples an
adversarial persona (`bench/inner_work/personas.toml`), seeds the fixed
resident-memory fixtures, and runs N turns of {35B brain proposes the
next user move → `Runtime::handle_message` on one `conv_id` →
length-blind judge}. Two-tier scoring, never averaged: a **safety
number** (% turns with zero Tier-0 red lines — crisis_mishandling,
harmful_validation, clinical_overreach, privacy_leak, retrieval_leak;
must reach ~100%) and a **witness composite** (% good among safe turns).
A hand-labeled calibration bank (`--calibrate`,
`bench/inner_work/calibration.toml`) gates any rubric change on breach
sensitivity ≥0.9. Journal + breach receipts:
`test-artifacts/inner-chaos-journal.jsonl` (stamped copy per run). Spec:
`sovereign/bench/inner_work/CHAOS_HARNESS.md`; runner:
`sovereign-cli-llm/src/inner_chaos/`. The witness's deterministic
wellbeing gate (`runtime/wellbeing.rs`) fires pre-routing on lexical /
classifier / sticky signals; since 2026-07-10 the sticky arm re-checks
the current message and hands non-crisis follow-ups back to the witness
(only an explicit classifier not-crisis releases; lexical hits and
classifier failures keep the guaranteed floor) — any edit there must
re-pass the `--persona crisis_discloser` suite.

**CI composition** (`scripts/sovereign-ci-bench.sh`) — **the primary way
to catch a regression anywhere in the inference + retrieval stack.** One
command spans the whole chain — retrieval recall, enrichment atom-F1,
intent routing, synthesis answer-equiv, tool-use (search/knowledge/agent
gyms), multi-turn degradation, chaos honesty, mechanism fidelity,
governance — by *composing* the benches above rather than reinventing
them, each diffed against a committed baseline. It is the regression net a
change to any of those subsystems must clear; the per-bench harnesses are
for drilling into a lane the gate flags, not the front-line check.
Deterministic baseline-diffed lanes (retrieval recall, enrichment
atom-F1, intent routing) are **hard** (build-breaking); the synthesis
answer-equiv judge lane is **soft** (judge variance shouldn't flake the
build); chaos, mechanism, the multi-turn degradation thread, and the
governance lanes run as **tracked** (advisory — their absolute verdict is
a finding about the current system, not a regression), each paired with a
**hard `svrn bench gate <lane>`** that re-scores the same artifact
and fails only on regression vs a committed baseline
(`sovereign/bench/<group>/baselines/<id>/`; first run passes). Gate logic
is one shared metric/direction/tolerance primitive
(`bench_cmd/lane_baseline.rs` + `gate.rs`).

Two modes: **`--quick`** is the pre-push lean tier — it down-samples the
slow lanes (`--sample-questions` on synth, one agent-coding problem,
`--max-turns` on the multi-turn thread bank) to a stratified, whole-unit
subset so signal-per-minute stays high (~35–40 min vs ~2–4 h); the full
run is the release/nightly gate. **Invariant:** a sampled lane's baseline
is *cap-specific* — it covers a different question/thread subset than the
full run, so changing a sample size or `MULTITURN_MAX_TURNS` requires
re-capturing that lane's baseline (`bench gate … --update-baseline` at the
new cap) or its hard gate false-fires against a stale baseline.

---

## 3. corpus-engine — the shared knowledge layer

Self-contained library; both upstream projects use it through the
same public API; neither knows the other exists.

### Pipeline

```
Acquirer → Extractor → Filter → Chunker → Embedder → Index
                                          (caller-supplied EmbedFn)
```

Each stage is a trait. A **Recipe** TOML configures the whole
pipeline. Built-ins per stage:

| Stage      | Built-ins                                                          |
|------------|--------------------------------------------------------------------|
| Acquirer   | `bulk_download`, `huggingface_dataset`, `local_file`, `http_api`, `web_crawl`, `custom` (runtime-registered seam) |
| Extractor  | `mediawiki_xml`, `stackexchange_xml`, `jsonl`, `json`, `markdown`, `xml_sections`, `wikipedia_jsonl`, `wikipedia_structured`, `wikipedia_catalog`, `wikipedia_api_article`, `gutenberg_catalog`, `html`, `html_sections`, `csv`, `parquet`, `plaintext`, `code`, `email` (RFC-5322 + MIME), `anthropic_export` / `chatgpt_export` (conversation imports), `alignment_workspace`, `custom`, `described_asset` (content-addressed binary dispatcher), `tabular_atoms` (deterministic typed Entity atoms per row from tabular JSON, e.g. the SF assessor parcel roll). The `ExtractorConfig` enum in `recipe.rs` is the SSOT. (`column_aware` — typed Entity atoms from parquet parsed-form caches — is an *enrichment-time* extractor configured via `[enrichment.reconciliation.column_aware]`, not a recipe `type =` value.) |
| Filter     | `pageview_rank`, `title_list`, `knowledge_density`, `boilerplate` (email signature / quoted-reply / disclaimer stripping), composed via `[[filter]]` (`Any` / `All`) |
| Chunker    | `paragraph`, `sentence`, `fixed`, `semantic`, `passthrough`, `portal_event_bullet`, `threaded_turns` |
| Index      | `CorpusIndex` over LanceDB (IVF-PQ) + Tantivy FTS                  |

The `tabular_atoms` extractor (deterministic, no inference) types each
row of a structured public dataset into an `Entity` atom whose columns
land in `Entity::attributes`. The SF land-value-tax demo folds those
atoms into aggregates via the `parcel_analytics` lib
(`enrichment/atlas/analysis/`) + the read-only `parcel_analytics` tool.
Its "no confabulated numbers" guarantee — *the model never originates a
number* — is enforced three ways: the model narrates only the tool's
COMPACT figures; the ComplexTask synthesizer appends the tool's
`derivation` VERBATIM (rendered by the system, not the model); and a
deterministic audit (`runtime::numeric_audit`) value-matches every $/%
figure in the prose against the tool's outputs. `svrn corpus
export-parcels` writes the input set to CSV for independent re-summing.
See `sovereign-recipes/sf-assessor-roll/`.

The `email` + `described_asset` extractors and the `column_aware`
reconciliation extractor (configured via
`[enrichment.reconciliation.column_aware]`, not an `[extractor] type`;
reads parquet parsed-form caches into typed Entity atoms) land together
as the substrate of the architecture-over-Enron push. Each future
binary-bearing vertical (Firm Inbox, sales intelligence, calendar /
transactions / sensor ingest) inherits the same dispatcher + asset-store
pair unchanged. See `bench/HISTORY.md`'s `enron-entity-resolution`
section.

### Storage

LanceDB vectors + Tantivy keyword. One on-disk dir per corpus;
identical schema for a full index or a shard.

```
~/.sovereign/indexes/
├── wikipedia/
│   ├── _corpus_meta.json                # authoritative metadata
│   └── chunks.lance/{...}
├── stackexchange-shard-0-6200000/       # same schema as a full index
└── enron-sample-onemailbox/             # architecture-over-Enron substrate paths
    ├── _corpus_meta.json
    ├── chunks.lance/{...}
    ├── assets/                          # AD-1 content-addressed asset store
    │   ├── ledger.jsonl                 # append-only LedgerEntry per sha256
    │   ├── <hh>/<sha256>                # raw bytes, sharded by leading 2 hex
    │   └── parsed/<sha256>.<ext>        # typed parsed cache (parquet/ical/…)
    └── atlas/
        ├── atoms.json                   # AtomsFile SCHEMA_VERSION 2.3 (canonical export)
        ├── atoms.lance/                 # ATLAS_STORAGE_V2 columnar atom store — the
        │                                # query-path reader (hot scalar columns + a
        │                                # lossless payload). Replaced atoms.rkyv; the
        │                                # sole atom backend. See docs/specs/ATLAS_STORAGE_V2.md
        ├── edges.csr                    # mmap'd CSR adjacency — sync, paged BFS
        ├── atoms_ann.lance/             # ANN seed table (atom_id → embedding), built at
        │                                # enrich/backfill; seeds atlas grounding (only
        │                                # on embedding-bearing corpora)
        ├── asset_atoms.jsonl            # AD-2 Asset envelopes (sidecar union'd
        │                                # into atoms.json on next atlas write)
        ├── asset_edges.jsonl            # EdgeType::Attaches edges
        └── reconciliation_oplog.jsonl   # Phase 4 reversible Merge/Split ops
```

`(corpus_id, chunk_id)` is the citation handle and is **structurally
unique** — `installed_indexes()` dedupes on `corpus_id` (prefers the
dir whose basename equals `corpus_id`, warns and drops collisions).
Out-of-band names (`.legacy-backup`, `.retired`) are excluded.
`IndexMeta` carries `ScopeMeta { filter_descriptions,
filter_signature, expandable }` plus an optional `filter_override`
so a corpus can be expanded in place (relax filters → delta-ingest
the additions → rebuild IVF-PQ).

### Injection contract

`corpus-engine` never embeds or generates text itself.

```rust
pub type EmbedFn     = Arc<dyn Fn(&str) -> Pin<Box<dyn Future<Output = Result<Vec<f32>>> + Send>> + Send + Sync>;
pub type InferenceFn = Arc<dyn Fn(&str) -> Pin<Box<dyn Future<Output = Result<String>> + Send>> + Send + Sync>;
```

| Caller       | `EmbedFn`                                | `InferenceFn` (enrichment)        |
|--------------|------------------------------------------|-----------------------------------|
| Sovereign    | wraps local Embed slot                   | wraps Main responder slot         |
| cmnwlth | `embed_http::http_embed_fn` → `/v1/embeddings` | mesh inference endpoint     |
| Tests        | zero-vector mock                         | canned-JSON mock                  |

Default embedding model: `qwen3-embedding-0.6b` (1024 dims, the
canonical `corpus_engine::DEFAULT_EMBED_DIM`).
`_corpus_meta.json` records the model; opening with a mismatched
model fails with `Error::IncompatibleEmbedding`. The Embed slot is
a cross-peer interoperability contract — nodes sharing a corpus
**must** produce bit-compatible vectors (`EmbedModelInfo` must
match).

### Sharding — three operations

| Operation                              | Effect                                          |
|----------------------------------------|-------------------------------------------------|
| `index_stats(corpus_id)`               | Total chunks, ID range, size on disk            |
| `extract_shard(corpus_id, range, dir)` | New index containing only chunks in range       |
| `merge_shards(dirs, dir)`              | Reconstitute a complete index from N shards     |

Shards are structurally identical to full indexes —
`CorpusIndex::search` doesn't know or care which it operates on.

### Per-node storage budget

Settings → Knowledge sets a ceiling. Enforcement lives at
`sovereign-mesh::capabilities::build_local_capabilities`, which
clamps published `free_storage_gb` to
`min(actual_free, max(0, budget − used))`. Every scheduler
(`assign_knowledge_shards`, the three
`plan_collaborative_ingestion*`) reads that one value, so the
clamp self-enforces for both local installs and peer-driven shard
distribution.

### Peer-assisted ingest ("Blanket") — one-time mesh help for personal sources

A personal source (Obsidian vault, watched folder, document folder) is
structurally `mesh_sharing=false` / `scope=local`, so no peer ever helps embed
or enrich it. Blanket lets the user hand a **chosen subset** of mesh peers a
**one-time, revocable, ephemeral grant** to shoulder that compute — the source
is never put into standing sharing, and nothing is retained by peers after the
job.

It rides the existing collaborative-ingest **work-queue** rather than a new
engine. Four additions on top:

- **Grantability marker** — `CorpusMeta.grantable` (`corpus-engine/recipe.rs`),
  set `true` ONLY by the three file-corpus recipe builders
  (`sovereign-tools/local_corpus/config.rs`). KnowledgeView corpora leave it
  `false`, so they stay structurally un-assistable even though they share the
  same `scope=local`.
- **Peer allowlist** — `CollaborateRequest.allowed_peers` intersects the
  embed-compatible candidate set; carried to peers via
  `IngestionHandoff.{allowed_peers, ephemeral}`. Enforced at enrollment
  (`sovereign-mesh/auto_ingest.rs`) AND in `WorkQueueManager::next_unit`
  (`QueueError::PeerNotAllowed` → 403).
- **Ephemeral grant** — `EphemeralGrantStore` (`commonwealth-knowledge/ingest_grant.rs`),
  in-memory, one live grant per corpus, renewable TTL (6h default, 24h cap). A
  single gate in `corpus_collaborate`: `mesh_sharing==true` proceeds as today;
  else requires `grantable && live-grant ⊇ requested peers`, else 403. The
  on-disk metadata is **never mutated** — that IS the "no standing share".
  Lifecycle routes: `POST /internal/corpus/grant`, `/grant/revoke`.
- **Teardown + verification** — coordinator broadcasts `partition_evict` to each
  peer after pulling its shard, and peers self-evict their partition dir on
  ephemeral-loop exit (two independent no-retention paths). Post-merge,
  `verify_merge_sample` re-embeds a sample locally and cosine-checks it against
  the peer-produced vectors (`shard_manager.rs`) — glassbox "re-checked N
  chunks, all matched".

Progress is polled via `GET /internal/corpus/collaborate/status`
(`CollaborateStatus` DTO). Desktop surface: `PeerAssistOffer` /
`PeerAssistPicker` / `AssistProgressPanel` (`components/mesh/`) driven by the
poll-based `assistProgress.svelte.ts` store, wired into the folder-drop flow
(one-shot), the watched-folder detail (standing grant), and installed-recipe
rows. The local ingest is never gated on any of this.

### Enrichment

**Three coexisting systems**, selected per-corpus by `[enrichment] type`
(the labeled `'enrichment:` dispatch block in `engine/ingest.rs`). See
[`corpus-engine/ENRICHMENT.md`](../corpus-engine/ENRICHMENT.md) — the
canonical umbrella that reconciles all three — before assuming "enrichment"
means one thing.

- **`field_model` — System 1, `enrichment/field_engine.rs`** — five-phase
  *whole-corpus* pipeline (skeleton → cluster → align → fault lines → open
  questions). `Domain` trait + `DomainRegistry`. Domains include
  `philosophy`, `multi` (Wikipedia), `personal` / `conversational` /
  `institutional` (KnowledgeView). Legacy but live (KnowledgeView digests,
  full-corpus SEP epistemic flow).
- **`atlas` — System 2, `enrichment/pipeline/`** — *per-document* typed
  atom graph (Entity, Claim, Event, Question, Position, Opposition,
  ArgumentReconstruction, Configuration, Asset). `Pipeline` trait +
  registry + `ExemplarBank` + `PhaseCache`. Pipelines: `literary`,
  `literary_atlas`, `philosophy_atlas`, `referential_atlas`,
  `engineering_atlas`, `conversation_atlas`. State at
  `~/.sovereign/indexes/<corpus>/atlas/`. Deep-dive:
  [`ENRICHMENT_V2.md`](../corpus-engine/ENRICHMENT_V2.md). Beyond the LLM
  pipelines, the deterministic **`structure_first`** strategy lifts a SCIP-indexed
  **code** corpus into this same typed-atom graph (content-hash atoms, code-intel
  summaries as descriptions, bounded function + call edges) — now **queryable**
  (multi-hop `AtlasGraph::call_chain` via `enrich atlas-query` + the chat) and
  **patchable** (`enrich atlas-patch-code` → delta → v2-store rebuild) on the v2
  `atoms.lance` / `edges.csr` store. See ENRICHMENT.md "Code as a queryable,
  patchable Atlas (v2)".
- **`tiered` — System 3, the RAPTOR + GLiNER gold standard** — three
  progressive tiers (T1 embeddings → T2 entity-graph + PPR → T3 RAPTOR
  cluster tree). The single RAPTOR builder lives in
  `sovereign-tools/src/raptor_atlas.rs` and is injected into `corpus-engine`
  via the `TieredEnrichmentProvider` trait (`enrichment/tiered.rs`) to
  avoid a cyclic dep. GLiNER (real ONNX NER) augments the conversation
  path — since 2026-08-03 (P2.1) via the generation-agnostic
  `LabeledEntityExtractor` seam, so v1 (gline-rs) and GLiNER2 (bare
  `ort`) share one persistence/dedup/provenance path.
  `SOVEREIGN_GLINER_MODEL_ID` picks which; it defaults to v1
  (`gliner_small-v2.1`) and GLiNER2 is dark pending the quality gate
  (`DEFAULTS_LEDGER.md`). The extractor that actually ran is recorded
  per corpus on `chunk_entity_progress` (`model_id`, `threshold`,
  `labels_json`). Used by attached docs, conversations, Obsidian /
  watched folders.
  `svrn enrich raptor <corpus>` (sovereign-cli-llm) retrofits this
  tier-3 tree onto an already-installed corpus additively — writes
  `conv_raptor_nodes` keyed by `source_doc_id`, reuses the existing
  leaf embeddings, and carries `--strip-furniture` + doc-level resume
  (the SEP whole-document-summarization retrofit, 2026-06-06).
  Bucketing is corpus-shape-aware (2026-06-11): vault/watched-folder
  corpora classify per-FILE units via `ConvBucket::classify_note`
  (Tiny only at 0-1 chunks — the conversation-tuned 8-chunk floor
  tiny-bucketed half the live vault into title-only synthetic nodes),
  while document corpora keep the conversation thresholds. For folder
  corpora the retrofit also finishes with `finalize_corpus` (vault
  synthesis + the typed-extension pass into `atlas/atoms.json` —
  see TIERED_RETRIEVAL.md's typed-extension section); document
  corpora never touch the atom-graph atlas. Query-time
  grounding (`apply_raptor_grounding`) reads those summary nodes via a
  derived per-corpus `raptor_summaries.lance` ANN index — built at the
  end of `enrich raptor` (or standalone via `enrich raptor-index
  <corpus>`) by `sovereign-tools/src/raptor_index.rs` over the pure
  `corpus-engine::index::raptor` primitives, with a `max(created_at)`
  freshness gate and the brute-force `conv_raptor_nodes` cosine scan as
  fallback (spec `docs/specs/RAPTOR_ANN_INDEX.md`).
  Deep-dive: [`docs/TIERED_RETRIEVAL.md`](./docs/TIERED_RETRIEVAL.md).

See [`corpus-engine/ENRICHMENT_V2.md`](../corpus-engine/ENRICHMENT_V2.md)
for status table, landing-by-landing scope, and validation targets.

### Recipe registry

Twenty-six catalog recipes ship in `sovereign-recipes` (§2), consumed
via `RecipeRegistry`:

- **Bundled snapshot** — `build.rs` vendors `sovereign-recipes/registry.toml`
  into `OUT_DIR` and `registry.rs` `include_str!`s it from there, so the engine
  works fully offline with no checked-in snapshot copy to drift.
- **Bundled fallback** — `recipe_builtin.rs::bundled_recipe_toml(id)` returns the
  full recipe TOML (also vendored from `sovereign-recipes/` into `OUT_DIR`) for
  snapshot entries when the live URL is unreachable.
- **Live refresh** — `RecipeRegistry::refresh()` pulls the latest
  from GitHub.
- **Resolution order** — local override on disk → remote → bundled.
  SHA-256 verified when the entry's `sha256` is non-empty.

### Recipe-authoring platform

The recipe schema is open. Domain experts (financial journalist,
legal aid attorney, grad student) author a TOML; the engine runs
it. Generic primitives:

- **`http_api` acquirer** — URL templating with `{name}`
  placeholders, four pagination strategies, JSONPath document-URL
  follow with bounded concurrency, token-bucket rate limit.
- **`[recipe.parameters]`** — String / Int / Date / List with
  defaults and required flags. `Recipe::resolve_parameters`
  validates user input.
- **`html_sections` extractor** — multi-regex section extraction
  with a `MissReport` sidecar so `recipe test` can show
  "section X missed in filing Y; nearby text: …"
- **Authoring-harness verdict layer** — `corpus_engine::harness`
  emits judgment-free `StageOutput`s; the `sovereign-authoring-harness`
  crate is the policy + presentation layer that folds them into the
  Pass/Fail ladder `recipe test` renders.
- **Investigation enrichment pipeline**
  (`enrichment/investigation/`) — recipe-declared
  `[[enrichment.entity_types]]` + `[[relationship_types]]` →
  JSON-Schema → llguidance grammar. Three built-in graph-pattern
  detectors (`circular_flow`, `role_overlap`, `threshold`).
- **Lifecycle** — `svrn recipe {validate,test,publish,list}`.
- **Agent-callable tools** in the studio crate
  `studio/crates/sovereign-recipe-author/` (re-exported as
  `sovereign_tools::recipe_author` for legacy paths) — eight Tool
  impls behind `Permission::RecipeAuthoring`. Wired into MCP via
  `MCP_TOOLS_ALWAYS` in `sovereign-tools/src/mcp_surface.rs`.
- **Recipe-author agent loop** — `sovereign-recipe-author/src/project.rs`
  (project model), `situated_context.rs` (per-turn renderer),
  `svrn recipe-agent {new,show,list,live-trial}` CLI. Skill
  manifest at `sovereign/modes/recipe-author/skill.toml`
  (privacy = `local_only`). The project model carries an
  `ArtifactKind` (recipe | workflow), so the same checkpoints /
  decision log / desktop workspace back **workflow** authoring too
  (checkpoints snapshot `recipe.toml` or `workflow.toml` by kind) —
  the recipes×workflows merge.

### Schema back-compat

Recipes live outside the repo (community registry, user
authoring), so a TOML written six months ago must keep loading.
Convention enforced by the reader + a regression-fixture suite
(`corpus-engine/tests/recipe_back_compat.rs`):

1. New fields carry `#[serde(default)]`.
2. Renamed fields keep the old name as `#[serde(alias = "old-name")]`.
3. Removed enum variants get a deprecation arm in
   `translate_parse_error` that emits "use `<replacement>` instead".
4. `[corpus] schema_version` bumps only when readers must opt in
   to interpret a recipe. Reader refuses recipes declaring
   `schema_version > MAX_SCHEMA_VERSION`.
5. Reserved variants — declare today, validator warns, runtime
   emits placeholder finding; later PR adds executor without
   touching the schema.

`[display]` (`recipe.rs::DisplayMeta`, `category` + `icon`) flows
through `_corpus_meta.json` and surfaces on `IndexInfo` so
retrieval reads category off the index summary without re-resolving
the recipe. Drives Atlas View rail grouping and the synth prompt's
"From your conversations" rename.

### Delta updates + safety

`update/delta.rs` — `VersionManifest` per-document revision IDs;
`ManifestDiff::compute` produces additions/updates/deletions;
three-phase apply (delete → update → add); `_update_progress.json`
for resume.

Hardcoded safety (not configurable per recipe): robots.txt
compliance, 1s/domain rate limit, UA
`CorpusEngine/0.1 (+https://sovereign.dev/corpus-engine)`, crawl
scope enforced against the seed URL domain, download size warnings
at 1.5× estimate.

---

## 4. Sovereign — the local agent

A single-machine local AI assistant. Runs as desktop, CLI, or HTTP
server against the same `Runtime`. No data leaves the machine
unless the user opts in to web search or a cmnwlth mesh.

### Trait architecture

`sovereign-contracts/src/traits.rs` (re-exported as
`sovereign_core::traits` — the contract crate carved out of
sovereign-core so packages build against the vocabulary without the
runtime hub):

| Trait                          | Surface                                                     |
|--------------------------------|-------------------------------------------------------------|
| `InferenceProvider`            | `complete`, `complete_stream`, `complete_stream_with_id`, `embed`, `embed_query`, `capabilities`, `code_model_id` |
| `Router`                       | `classify(message, ctx, tools) → RouterClassification`      |
| `Planner`                      | `plan(goal, context, tools) → Plan`, `replan(...)`          |
| `Tool`                         | `descriptor`, `execute`, `validate`, `retry_config`, `required_permissions` |
| `LandscapeDigestProvider`      | `splice_landscape_digests(ctx, active_skill)`               |
| `ApprovalChannel`              | Human-in-the-loop tool approval (CLI / Tauri / Server / Auto) |
| `MeshKnowledgeSource`          | Fan-out knowledge search to mesh peers                      |
| `SensitiveCorpusOracle` / `FolderMetadataOracle` | Watched-folder privacy + UI surface |
| `InsightStore` / `InsightSink` | Long-term insight extraction + persistence                  |

`StateStore` is decomposed per ISP into 12 focused sub-traits
aggregated by a single blanket impl: `ConversationStore`, `TaskStore`,
`MemoryStore`, `RoutingStore`, `DocumentStore`, `CorpusStateStore`,
`BudgetStore`, `PermissionStore`, `StepExecutionStore`, `HealthStore`,
`DocumentSessionStore`, `DocumentAssetStore`. (`InsightStore` is a
standalone trait — impl'd by `SqliteInsightStore`, not part of the
aggregate.) Callers narrow bounds to what they need. `StepExecutionStore` is the
durable per-attempt ledger behind executor replay-safety (below);
its methods default to no-ops so non-durable mocks are unaffected.

### Runtime data flow

```
User message
  → Router.classify           (Quick slot, two-pass coarse → refine)
       → RouterClassification { primary, alternatives, rationale, … }
  → decide_policy(classification, ConfidenceThresholds)   (pure fn)
       → RoutingPolicy { tier, move_kind: Commit | Propose | Ask, … }
  → SessionStore.begin → QuerySession (CancellationToken-bearing)
  → Dispatch by Intent:
       ├─ SimpleQuery / DeepQuery / KnowledgeQuery → search → synthesize
       └─ ComplexTask → Planner.plan (Main slot)
                      → Executor (topological batches)
                          ├─ ReasonWithTools loop
                          ├─ Best-of-N sampling (LlmJudge / Random / Best)
                          ├─ Evaluation passes
                          └─ Tool steps with permission + approval
  → Provenance recorded into Message.metadata
  → Memory extraction at conversation end
```

`Plan` is a flat JSON DAG (`steps`, `edges`). `StepKind`: `Reason`,
`Tool`, `UserInput`, `Branch`, `ReasonWithTools`, `AwaitUserInfo`,
`Delegate`. `Step.sampling`/`Step.evaluation` are typed fields (the
planner leaves them `None` today; the executor synthesizes defaults in
`compute_budget`). The one textual grammar the planner and executor
share is the `{N.key}` step-output placeholder, owned end-to-end
(emit + parse + prompt-sync test) by `sovereign-core/src/plan_grammar.rs`.
(A previous revision of this paragraph described `[sample:N:method]` /
`[eval:name]` annotations — that grammar never existed in code; the
2026-07-12 hidden-coupling audit corrected it.)

**Idempotency ledger (executor replay-safety).** Before a
`NonIdempotent` tool step runs, the executor writes a durable `Started`
`StepExecution` row (`StepExecutionStore`) keyed by a content-derived
idempotency key (`task:tool:hash(params)`), flipping it to `Completed`
after the side-effect returns. On resume the guard reads that key: a
`Completed` row means the action already ran — a replan re-runs from an
empty completed-set, so this is the path that would otherwise re-send —
and is skipped with its recorded result; a `Started`-but-not-`Completed`
row means a crash interrupted it mid-flight, so the executor halts and
surfaces for review rather than blind-replaying a non-idempotent
side-effect (an email sent twice). The key is content-derived, not
`(task, step_id)`, so it matches across a replan that re-issues the same
action under a new step id. Proven exactly-once both ways by
`sovereign-store/tests/step_execution_replay.rs`.

**Delegate — the context-firewall worker (§5.2).** `StepKind::Delegate {
goal, tools, return_schema, max_iterations }`
(`executor::Executor::execute_delegate`) runs a scoped rich-param tool loop
in its OWN context: the worker drives the requested tool subset via the
`<tool_call>{"name","arguments"}` protocol, the raw observations (a page
DOM, a sheet's cells) accumulate in the worker's local transcript, and only
a typed contract — the `return_schema` fields plus an always-present
`anomalies` channel — flows back to the orchestrator. So the planner
decides on a compact summary, never a wall of raw output. The
`{name, arguments}` parser + tool-schema projection are **shared** with the
recipe-author loop via `crate::tool_loop` (one parser, no drift) — distinct
from the search-shaped `{tool, query}` loop in `executor.rs` that
`ReasonWithTools` uses. Firewall proven by
`sovereign-store/tests/delegate_firewall.rs`. (v1: the worker's internal
tool calls go straight to `tool.execute`, bypassing the idempotency ledger
above — threading #4 into the worker loop is a follow-on.)

**TEACHABLE lessons — coach in chat, own in settings** (design:
`sovereign-desktop/TEACHABLE.md`). The behavior lane: a durative
coaching turn ("keep answers shorter **from now on**") routed to
ConationQuery forks a detached capture spawn
(`runtime/handlers/conation.rs` → `sovereign-core/src/lessons.rs`) that
compiles the intent DETERMINISTICALLY to the cheapest enforcement rung
(param → transform → prompt; the fast slot phrases only prompt-rung
lessons) and emits a fire-and-forget `lesson-proposed` card — consent
is stateless, dismissals store nothing. Saved lessons are notes
(`kind = "lesson"`, corpus-engine-notes MIGRATION_V11; payload schema =
`lessons::LessonPayload`, source fields `{display, taught_from}` vs
derived `{prompt_form, enforcement, params}` stamped
`compiler_version`), one ACTIVE lesson per rung (the desktop
`save_lesson` command supersedes-and-retires). Enforcement: rung 1
clamps the output-budget SOFT target (never `max_tokens`); rung 2 is a
whole-word term-avoid pass running post-grounding-gate and
post-citation (structurally unable to touch `[Source: …]` anchors);
rung 4 appends ONE compiled sentence outermost on the system + refine
prompts of primary synthesis intents only. Every influenced turn
records `metadata.lessons_applied`; the first records
`metadata.kept_lesson` exactly once (the whisper). No rung touches the
grounding gate — facts are scored by evidence provenance, never
preference. Settings surface: `LessonsPanel.svelte` ("What I've
learned"); measurement: `personas.mjs --coach` A/B report + the
zero-tolerance `capture_precision` gate.

The router emits **facts**; the runtime applies **policy**.
Splitting them keeps classification testable without a model and
lets thresholds calibrate without touching the trait.

**Router classifier stack — one wiring path, all surfaces.** Before the
coarse→refine LLM cascade, `Router.classify` consults a stack of
embedding-centroid pre-checks: the **embed router** (intent exemplars → a
direct intent when confident, skipping the LLM passes), the **scope**
classifier (personal vs external), the **effort** classifier (a high-effort
referential `Answer` → `DeepQuery` → primary slot — the exhaustive-ask
escalation), the **current-info** classifier (drives `force_action` for
time-sensitive queries), and the **archive** classifier (past chats vs this
thread — see below). All five are assembled by the single helper
`sovereign-core/src/router_bootstrap.rs::build_llm_router`, which **every**
surface calls — CLI/bench, desktop, and the served daemon. Exemplars are baked
into the binary (`include_str!` of `sovereign/router/*.toml`), so the stack
works regardless of CWD or `.app`-bundle layout; a `SOVEREIGN_*` env var or
repo-relative file overrides the baked default. This is **parity by
construction**: every surface gets the same stack because there is only one
wiring path, and `tests/router_bootstrap_parity.rs` asserts `all_wired()` so
the surfaces can't silently re-diverge. (How the stack once diverged —
desktop and daemon silently under-routing while the benches improved — and
was collapsed: [HISTORY](./HISTORY.md#router-stack-parity-2026-06-09).)
Effort-tier escalation +
robust coarse-verdict recovery default **ON** (`SOVEREIGN_KQ_EFFORT_TIER=0` /
`SOVEREIGN_ROUTER_ROBUST_COARSE=0` disable).

**The locator axis — "is this question about our conversation?"
(2026-07-26).** A third orthogonal axis on the exemplar bank, alongside
`scope`. Rows tagged `locator = "conversation"` in
`sovereign/router/exemplars.toml` are scored **one-vs-rest**
(`EmbedRouter::locator_from_embedding`): best similarity to a tagged row
minus best similarity to every other row in the bank, gated on its own
floor + margin, decided **independently of the intent gate**. It exists
because the two axes disagree on exactly the queries that matter — "what
was the first thing I asked?" is intent-ambiguous (it sits near conation
and near personal-archive recall) while being locator-unambiguous — and
because reading a tag off the winning *intent* exemplar is the design
that already failed once for `scope` (`scope_classifier.rs` post-mortem).
The verdict runs as **Pre-check -2.5** in `router.rs`, ABOVE the
knowledge-thread-inherit pre-check that used to swallow these questions,
and the query embedding it pays for is threaded down to the intent and
scope classifiers so the per-turn embed count stays at one. Committing
hard-commits `MetalingualQuery` with coarse label
`CONVERSATION_LOCATOR_EMBED`, and that label — not a second string parse
— is what tells `handle_metalingual_query` which locator to honour
(`locator_hint_from_coarse`). Before it, the Conversation family was
gated on nine literal substrings, so any other phrasing inherited the
thread's knowledge intent and searched a corpus for an answer sitting in
the message list. Thresholds are calibrated, not guessed: zero false
positives over a held-out negative set that includes the adversarial
neighbours (archive recall over *past* conversations, world questions
using ordinal/summary vocabulary) — re-runnable against a live daemon
via `tests/locator_axis_live.rs --ignored`.

**The archive axis — "past chats, or *this* one?" (2026-07-26).** The
locator axis above answers "is this about THIS thread?"; this one answers
the adjacent question its negative set was swept against. "Have I
mentioned kayaking in any of our past chats?" used to route
`MetalingualQuery`, whereupon the handler string-parsed the locator to
`Unknown`, preferred CODE corpora, found nothing, and emitted the
`no_source` empty state — the user's own archive never searched. The
correct verdict was *already* top-ranked (`KnowledgeQuery`, scope
personal, 0.531) but sat under the 0.55 intent floor, so the router
abstained and the LLM classifier picked metalingual. It is not wrong to
call the question conversational; it is wrong about *which*
conversation.
Two cheaper fixes were rejected with numbers: more exemplars (similarity
in the bank is topic-dominated — the same exemplar scores 1.000 on its
own topic and 0.531 on a different one), and a rule over the existing
axes (cells_v1's own metalingual row scores *more* negative on the
locator axis than the archive query does, so any threshold catching one
flips the other). So archive-vs-thread gets its own centroid classifier
(`archive_classifier.rs`, `sovereign/router/archive_examples.toml`) in
the shape that worked for `scope`, and runs as **Pre-check -2.4** —
*after* the locator axis, so the older and more heavily swept gate wins
any disagreement. Firing hard-commits `KnowledgeQuery` **plus
`scope = "personal"`** (coarse label `CONVERSATION_ARCHIVE_EMBED`);
without the scope the intent alone would search Wikipedia for the user's
chat history. Calibrated on the shared instruction-prefixed embedding —
the unprefixed space collapses world negatives into the positive range —
at 5/6 held-out positives and **0/20 false positives**, re-runnable via
`tests/archive_axis_live.rs --ignored`.

**Threshold calibration — `svrn router fit` (2026-07-28).** The six gates
above ship **twelve hand-picked constants**, each calibrated against
Qwen3-Embedding-0.6B and justified in prose. Two of those decisions turned
on thousandths — an archive negative held out "by only 0.002 of margin", a
tool gate hijacked by "0.011 of cosine noise" — and both were found days
late, by hand, from a bench regression. Nothing said which of the remaining
constants was one embedding-model change from the same fate.

Three pieces close that. **(1)** `router_axis.rs` extracts the shared
decision rule — `AxisScore{sim_positive, sim_negative}` + `AxisGate{min_sim,
min_margin}` + `cushion()`, the signed distance to the boundary — and
*separates scoring from gating* on all six axes
(`score_from_embedding` / `classify_*`). That is what makes a threshold
sweep pure arithmetic: one embedding pass over a bank makes the whole
threshold space searchable. **(2)** `router_calibration.rs` sweeps it
exhaustively, with candidate thresholds at the **midpoints between observed
scores** rather than the prior art's random linspace — exact optimum, maximum
headroom, and never a threshold placed *on* an observation (in f32,
`0.50 - 0.46 = 0.0399999…`, which does not clear a 0.04 gate: the
subtraction moves the boundary, not the comparison). Objectives are
`SafeRecall` (default — it encodes the asymmetry every axis documents),
`Accuracy` (for prior-art comparison) and `MaxCoverage` (the intent axis).
**(3)** `sovereign/bench/routing/calibration/axes_v1.toml`, a bank
deliberately authored to **fail somewhere**: 74 cases, every one carrying a
`note`, of which 32 are `expect = "abstain"` — the repo previously had no
abstention test anywhere.

Two guards make the tool refuse to lie about its own power. A margin floor
is **clamped to ≥ 0**: an unconstrained sweep over four cases happily fitted
archive to `-0.101` and scope to `-0.152`, gates that score perfectly and
fire when the *negative* class won. And `FitReport::underpowered()` flags
any axis with fewer than five cases in either class
(`MIN_CASES_PER_CLASS`), printing "read the shipped numbers, not the fitted
gate" — the fit-on-your-own-test-set failure the prior art commits by
reporting its headline on the same 66 rows it tuned on. **The command
writes no constant**; it names the constant and the file and stops.

**Per-case attribution (`--explain`).** A confusion matrix says *two false
positives*; the next question is always **which two**, and until 2026-07-29
nothing could answer it — `evaluate()` incremented counters and dropped the
case id, so the operator's only recourse was to re-derive the buckets by
hand. `ScoredCase::verdict()` is now the single bucketing rule, and both
`evaluate()` (which counts verdicts) and `attribute()` (which names them)
route through it — so the per-case listing can never contradict the totals
printed above it, and a test sweeps every reachable gate on a bank asserting
the two views agree. `verdict_changes()` adds the other half: `would_change()`
says *that* moving a constant changes something, this says *what*, per case.
`--explain` prints the errors behind each axis (expensive first, closest to
the boundary first) plus the flips a move would cause; `--format json`
always carries every case for both gates.

The first run paid for itself. The locator axis's 2 false positives are
`loc_abstain_last_week` and `loc_abstain_across_all_chats` — both
**archive-recall** questions, and both scoring *higher* on the conversation
locator (sim 0.668 / 0.660) than any of the three true positives the gate
misses (0.420–0.448, at **negative** margins). No threshold reaches those
three: the geometry ranks archive questions as more "about this conversation"
than genuine in-thread ones. That is an **exemplar-coverage** defect, not a
threshold defect, and the fitted `(0.314, 0.178)` gate only clears the two
FPs by threading a 0.031 band above their margins — a fix tuned to two
specific cases. The same view shows 7 of the intent axis's 13 misses already
predict the **correct** label and are held out by the floor alone.

**Rival attribution — naming what a case lost to (2026-07-29).** Knowing a
case is an error is not knowing how to fix it. A missed positive at
`margin -0.133` was beaten by *something*, but `score_locator_from_embedding`
computed the negative side as a bare `f32::max` and threw the identity away,
so the only available move was to guess more exemplars — the guess
`archive_examples.toml` already records failing (similarity there is
topic-dominated; "adding rows buys the topics you add, nothing else").
`LocatorScore`/`IntentScore` now carry `rival_exemplar` — the untagged row
that set `sim_negative`, or the runner-up intent's nearest — and it flows
through `ScoredCase`/`CaseAttribution` into `--explain` and the JSON. The
production glassbox log (`target: "router.locator"`) carries it too, so
"why did the locator abstain?" is answerable from logs alone. The centroid
axes (scope, archive, current_info, effort) leave it `None` honestly: their
positive class is a mean over ~20 rows and no single row is responsible.

It reclassified the finding a second time. The three misses lost to three
*different* rows: two to `conation_query` exemplars ("Elaborate on the second
point.", "Walk through what you just did, step by step.") and one to an
`expressive_query` row ("I'm not sure if I'm doing this right."). Both false
positives traced to a **single tagged row** — ordinal exemplar A, then
phrased "What did I ask you at the very start of this chat?", whose meaning
lives in a clause the encoder does not weight while the clause it does weight
is one every archive question also says.

That mattered because the locator **hard-commits**: Pre-check -2.5 returns
`MetalingualQuery` with no classifier vote, and it sits *above* the
conversation-archive axis at Pre-check -2.4. An archive question the locator
claims can never reach the classifier built to catch it, so the locator has
to reject archive shapes itself.

The repair moved **no constant** — reword A off the shared surface, add A2 to
keep the canonical positive it was carrying, add tagged rows for three
uncovered shapes, add two archive-recall negatives under `scope = "personal"`.
On the same bank: **errors 5 → 1, false positives 2 → 0, correct fires 2 → 4,
accuracy 50% → 90.9%**, and the shipped `(0.500, 0.020)` gate is now optimal.
The cost is headroom, recorded because it is what moves next: separation
0.142 → 0.066, weakest-accept +0.114 → +0.038, since the axis now decides
four cases where it decided two.

The surviving miss is a **measured encoder limit**, not a gap. The bank was
split to pin a boundary it had been asserting one side of: ordinal *recall*
("what was the second option you listed?") versus ordinal *resume* ("go back
to the second option you listed"), the latter an abstain under the same rule
as `loc_abstain_summarize_pasted` — a transformation of in-context content,
which a hard commit would answer with a recitation. They land 0.018 apart
**with the negative ranked higher**, so no threshold fires one without the
other, and both lose to "Elaborate on the second point." Recall-vs-resume on
an ordinal reference is below this encoder's resolution; the axis abstains on
both, which is the safe direction.

**Effort axis — why "grow the bank before moving the constant" is a rule
(2026-07-29).** A fit against the original 10 effort cases proposed
`(0.300, 0.040) → (0.482, 0.025)` and it looked *strictly dominant*: same 5/5
correct fires, the single false positive cleared. Moving the constant on that
evidence would have been wrong in both directions at once. The bank was grown
to 18 with cases chosen to **break** that gate rather than confirm it, and on
the larger bank `(0.482, 0.025)` scores **6 correct fires and 1 false
positive** — it rejects `eff_diagnose_latency` (sim 0.418, a true HIGH) on the
raised floor while `eff_abstain_thorough_but_trivial` (sim 0.556) sails
through it anyway.

The real separation was never in the floor. Sorted by margin, seven genuine
HIGH cases sit at +0.099 and above, both false positives at +0.057, so
`DEFAULT_MIN_MARGIN` moved 0.040 → **0.078** — the midpoint of that gap,
0.021 clear on each side — and `DEFAULT_MIN_HIGH_SIM` stayed at 0.30. On the
18-case bank that is errors 4 → 2, false positives 2 → 0, correct fires
unchanged at 7/9, and the shipped gate is now optimal.

The load-bearing new case is `eff_short_but_hard` ("think this through
carefully, then give me your answer in one paragraph"). The old bank tested
*long answer, low effort* (`eff_abstain_exhaustive_but_shallow`) but never the
inverse, and every shipped HIGH exemplar is an exhaustive expository essay —
so an axis that had learned **length** rather than reasoning depth would have
scored perfectly. It fires correctly, which is the first actual evidence for
the axis's own stated claim. The two remaining misses
(`eff_counterfactual_architecture`, `eff_tradeoff_pick_one`) rank *below* both
false positives, so they are an exemplar-coverage gap — neither is an
expository essay — not a threshold one.

**Intent axis — the coverage ceiling is the geometry, not the objective.** A
prior reading held that the axis's coverage came from
`MaxCoverage{min_precision: 1.0}` refusing any mislabel trade, and that
relaxing the floor would buy coverage cheaply. That is refuted, and remains
so: every precision floor from 1.0 down to **0.7** returns the same gate.
Sweeping every gate exhaustively (thresholds at each observed score and each
score plus epsilon, so gates that *exclude* a given case are reachable) is
what settles it, and the sweep is replicable from `--format json` — the
per-case `attribution` block carries `sim_positive` and `margin` for every
case, which is all a re-derivation needs.

Rival attribution then surfaced two *exemplar-level* defects. **One was real
and is fixed; the other is not exemplar-fixable at all.** Both results are
worth more than the coverage number they moved.

*Fixed — a taxonomy rule the file stated and its own exemplars violated.*
`int_code_chunker_type` (want `code_query`) was losing to
`"Where is the chunking strategy defined here?"`, tagged `metalingual_query`.
That is not an open taxonomy question: `exemplars.toml` **states** the
discriminator — `code_query` is WHERE it lives · WHAT CALLS it · HOW it runs
(answered from the SCIP call graph), `metalingual_query` is what a term MEANS
· what CHANGED (answered from prose). The 2026-07-25 migration wrote that rule
and added exemplars under it but never swept the pre-existing metalingual
block, leaving two "where does it live / how is it implemented" magnets on the
wrong side. Re-filing exactly those two (2026-07-29) moved the whole Pareto
frontier, on the *identical* 24 cases:

| correct fires | 2 | 3 | 4 | 5 | 6 | 7 | 9 |
|---|---|---|---|---|---|---|---|
| min hard errors, before | **0** | 2 | 2 | 4 | 4 | 7 | 13 |
| min hard errors, after | **0** | **0** | **0** | **0** | **1** | **3** | **8** |

Free correct fires went 2 → 4 (both endpoints confirmed by the fitter's own
safe-recall objective, not only by the replication). Errors 16 → 15, mislabels
2 → 1, precision 40% → 60%. `int_code_retry_owner`'s margin more than doubled,
0.058 → 0.127. No constant moved, and the other five axes came back at
**+0.000** separation — the edit is confined to the axis it was aimed at.

*And it carries a side effect worth generalising.* A/B'd by flipping the two
tags back, rebuilding the cache and re-fitting: **consolidating two classes
that shared a neighbourhood inflates the margin for everything in it.** Margin
is a *relative* measure, and the absorbed class was the runner-up holding it
down. No similarity changed — only which class claimed the win — yet
`int_abstain_chunker_overlap_hunch`, a query that should abstain, went from
margin +0.043 to +0.128. At the 0.100 margin shipped at the time, the re-filing
*created* that false positive rather than relabelling a pre-existing one, which
is what the plausible argument from "the same exemplar wins either way"
predicted. It is harmless at the 0.206 margin shipped now, and the move is a
clear net win there (2 correct/1 mislabel → 3 correct/0 mislabel). **The rule:
after re-filing exemplars between classes, re-check the abstain cases, not just
the positives.**

*Not fixable by exemplars — topic dominates shape.* `int_cmp_rawls_nozick`
(want `comparison_query`) loses to a `deep_query` exemplar about **Rawls**.
The tempting reading is that `comparison_query` learned its marker verb rather
than its stated shape — all fifteen exemplars open with
compare/contrast/differ/difference, so nothing separates the two hypotheses.
It was tested: three marker-free two-entity exemplars were added, the cache
rebuilt, and **every case came back byte-identical**. The control case
`int_cmp_hawk_dove` sits at sim 0.357 with `comparison_query` not even the
runner-up. The exemplars were reverted; the control case is kept so the
refutation stays measurable. The axis is k=1 over 11 topically-overlapping
classes, so the only exemplar that can win the Rawls case is one *about
Rawls* — which is coaching to the bank. Moving this ceiling needs per-class
thresholds or a topic-normalised score, not more exemplars.

*The bank grew to adjudicate a floor move, and reported something worse.* The
floor looked like a live lead: (0.45, 0.100) measured +4 correct fires for no
new error, and `fit` could not surface it because safe-recall refuses any
mislabel while the shipped gate already carried one — so the fitter judges
candidates against a stricter standard than the status quo and cannot propose a
gate that ties shipped errors while raising coverage. (`--max-false-positives`
does not relax this; it governs abstain-fires only. That objective defect is
real and still worth fixing.)

Growing the bank 27 → 40 to test that move found the **coverage bias that had
been hiding the axis's actual precision**: all seven original abstain cases
were 2-6 word ellipticals ("go on", "tell me more"). A floor drop admits the
sim band 0.45–0.55, and only *content-rich* queries score there — so the bank
was proving the floor safe using exactly the cases a floor cannot endanger.
Four long, confident-sounding, genuinely under-determined abstain cases were
added. Two of them fire, **at the shipped gate, not the proposed one.**

The shipped gate's real score on adequate evidence was **5 correct fires
against 5 hard errors — 50% precision.** The axis was committing wrong about as
often as it committed right, against its own documented asymmetry (a false
positive hard-commits the turn; a false negative merely falls through to the
cascade ~1.2s slower).

So the repair is the **margin**, not the floor: `DEFAULT_MIN_MARGIN` 0.10 →
0.206, floor untouched. **3 fires, 0 mislabels, 0 false positives — 100%
precision**, separation 0.018 → 0.033, and `fit` now reports the axis
**optimal** rather than movable. The margins sort with a clean gap exactly
there: every hard error at ≤ 0.190, every surviving correct fire at ≥ 0.223.
Five hard commits removed for two extra cascade calls. The floor is left at
0.55 having been shown *inert* — every point on the frontier is reachable at a
floor of 0.000 — because moving a constant that screens nothing is pure risk.

*Why coverage cannot be recovered by tuning.* Below 0.206 good and bad
interleave with no separating value: the two worst false positives fire at
margins of 0.171 and 0.190, **higher than most correct fires**. On this axis
margin measures confidence, not correctness. The purpose-built pair proves
why — `int_know_losalamos_arrival` (a bare date lookup) and
`int_deep_losalamos_disillusion` (a causal question) resolve to the *same*
nearest exemplar, and the wrong one wins with the bigger margin. That is the
same topic-dominance that refuted the `comparison_query` fix, now demonstrated
under control rather than inferred. Recovering coverage needs per-class
thresholds or a topic-normalised score — not a threshold move and not more
exemplars.

**Score-distribution drift (`router_drift.rs`).** `fit` is a snapshot, and
the failure this system actually has is that the ground moves while the
constants stay still — a new encoder, a re-quantised one, an edited exemplar
bank shifts every cosine without touching a line of `scope_classifier.rs`.
So `--save-baseline` records the run as a dated `FitSnapshot` under
`sovereign/bench/routing/baselines/<bank>-fit/`, reusing `bench all`'s
existing dated-JSON + `latest.json` convention rather than adding a metrics
pipeline, and every later run diffs the shipped gate's **cushions and
separation** against it. A regression is only *claimed* when the encoder and
the bank are both unchanged (both recorded — the bank by content digest);
otherwise the deltas print as evidence and the report says why they are not
attributable. A moved constant is named rather than blamed, unless the edit
cost errors. Exit codes: `0` clean · `3` a gate is movable · `4` drift
regression.

The **first baseline is recorded** (2026-07-28) against the prescribed
`Qwen3-Embedding-0.6B-Q8_0.gguf` over all 74 cases. Why the *prescribed*
model and not whatever is to hand: the same bank scored under the f16
`qwen-embedding-0.6b.gguf` moves `separation` by **0.000–0.004** per axis —
small, and **no decision changes** (identical errors and coverage on all
six), so the two quantisations are interchangeable *for routing*. But 0.004
is four times `DRIFT_EPS`, so treating them as one encoder would manufacture
a "regression" on four axes the first time anyone switched. Hence the
comparability key stays the exact model file, and the baseline is pinned to
the one production runs. (The equivalence itself was measured with this
tool — a cross-encoder run prints the deltas as evidence precisely so
questions like this get answered with numbers.)

**Routing metrics past exact-match (`eval_cmd/routing_metrics.rs`).** The
five routing banks score 96/96, so accuracy stopped being informative.
`RoutingMetrics` adds what accuracy hides: **layer attribution** (which
decisions the embed router owned versus which woke a ~1.2–2.4s LLM call —
the number a threshold fit can actually move), per-intent
precision/recall/F1, and ranked `expected → actual` confusions. It renders
under `eval run --routing-only` and its one-line `headline()` carries into
the `bench all` rollup. Deliberately **no abstention metric** there: the
full cascade always returns an intent, so abstention is not observable at
that layer — it is a property of the individual gates, and it is measured
where it exists, by `router fit`.

**Pre-built router-embed cache (`router_embed_cache.rs`).** The five boot
classifiers embed ~350 static exemplars at every process start — ~5.7s on Apple
Silicon, *minutes* on a CPU-only embed slot (Intel Macs, which `embed_slot.rs`
gates off Metal). So the embeddings are pre-computed for the prescribed embed
model and committed at `sovereign/router/router-embed-cache.json`, baked into
the binary (`BAKED_ROUTER_EMBED_CACHE`) and loaded as the fallback when
`~/.sovereign/router-embed-cache.json` is absent — first launch HITS instead of
re-embedding. A sentinel cosine probe validates it against the live model, so a
genuinely swapped embed model is rejected → one-time re-embed, surfaced as the
`RebuildingRouterEmbeddings` bootstrap phase. Freshness is a pure no-inference
gate (`check_cache_fresh`: exemplar-key coverage + a `family|hf_url` model
fingerprint) enforced by `tests/router_cache_fresh.rs` (CI + a
`desktop-release.yml` pre-flight) and regenerated by `svrn router-cache
rebuild`, which `scripts/bump-desktop-version.sh` runs when stale. The same
`EmbedSlot::load` auto-detects the Qwen3-Embedding family from the gguf
architecture, so the prescribed model under a non-default filename still gets
last-token pooling + the query instruction-prefix (and thus matches the cache).

**Synthesis role layer (`role.rs`).** The knowledge-turn path is organized as
three data-defined roles — the synthesis-side counterpart to the agent-loop
roles in `commonwealth-agent-tools/src/role/`, lifting the same
`RoleProfile`/`RoleModelMap` shape (ARCH §6: profiles are *data*). **Router**
classifies + resolves the route (mechanism: `EmbedRouter` +
`resolve_synthesis_route`); **Synthesizer** assembles the grounded answer
(mechanism: `build_synthesis_system_prompt` — the one prompt-body builder all
synthesis sites now call); **Critic** is a *separate verification pass*
(mechanism today: the bench grounding/abstain/caveat classifiers — defined in
`role.rs` so bench + any future prod critic share one definition, **not yet
wired into prod synthesis**). Each `RoleProfile` ships with its
`verify_predicate` (the keystone: the predicate defines correctness, the bench
measures it). Two SSOT decisions back this: `build_synthesis_system_prompt`
(one prompt body, byte-equivalence-tested) and `resolve_synthesis_route` (the
single traced FastFocused-vs-PrimarySynthesis decision with a typed
`RouteReason`, truth-table-tested against the legacy ladder) — together they
end the "live path mis-identified three times" illegibility. `role.rs` is
**load-bearing**, not just declarative: the resolver returns `role::Tier` (via
`SynthesisRoute::tier()`, surfaced in the KnowledgeQuery trace as
`role=synthesizer tier=…`), and the chaos bench sources the Critic's gate model
from `default_profile_for(Role::Critic)` (`--critic-model`, default primary).

**The production grounding gate (`runtime/grounding/`).** The gate's
load-bearing design decision is the **evidence universe**: it verifies claims
against the *sealed corpus* (per-claim hybrid search via `ClaimSearcher`), not
just the prompt snapshot, and feeds failed claims' corrective passages into
the rewrite (replace, don't delete). That choice is what makes a gate
net-positive at all — an earlier Critic-as-gate was empirically ruled out,
then the verdict reversed by widening what the judge could see
([HISTORY](./HISTORY.md#the-grounding-gate-verdict-reversal-2026-06-09--06-11)).
It PASSES the full bank (secret-agent 0.67/0.82/0.18 production-config;
holdout honesty 0.91/0.09). Mechanism: **hold → verify → corrective retry
(short answers) / per-claim audit → rewrite → annotate (long-form) → grounded
abstention**, fail-open on judge failure. Judge prompts are byte-pinned to the
bench critic so the bench-calibrated τ=0.9 transfers. Module layout:
`grounding/config.rs` (`GateSurface` closed enum + per-surface
`GroundingProfile` budgets + `grounding_gate_flags()` registry), `judge.rs`
(claim extraction, forced-choice support, joint long-form judge), `search.rs`
(`SealedEvidenceSearch` trait — claim-conditioned widening that can never
widen corpus scope), `mod.rs` (the ladder: `gate_answer` over an
`EvidenceContext`), plus `citation.rs` / `citation_attribution.rs` /
`value_presence.rs` (citation forcing + numeric-presence checks).
**Live gate progress (the verification counter, 2026-07-15).** On the two
streaming surfaces the ladder also narrates itself: `gate_answer_with_progress`
try_sends `NarrationPhase::{ClaimCheckStart, ClaimVerdict, ClaimRevisionStart,
ClaimCheckComplete}` frames (never backpressure — drop-on-full) through a
channel `gate_held_answer` forwards as `turn-narration` events, and
`RetrievalComplete` now carries `top_titles`. The desktop renders these as
`CounterCard.svelte` — a Gather → Draft → Check station card that replaces the
chip stack + promoted narration line during a gated hold and stamps each claim
as it verifies (reducer: `applyCounter` in `routing.machine.ts`; e2e:
`counter-card.spec.ts`). Every element is frame-driven; the card never invents
progress, and retrieval-only signal is provisional — the moment tokens stream
with no gate signal (an ungated turn) the card yields to the legacy
indicators. On serve, a quiet **verification receipt** persists on the bubble
(`AssistantMessage` `verification-receipt`, from `grounding_gate` meta —
release actions only, never fail-open verdicts). The **attached-doc surface is
wired too**: `gate_attached_doc_answer` opens the same progress channel
(`GateProgressWiring::spawn_reader`), the counter outranks the
`document:operation` progress line once claim frames arrive, and
`DocumentAskResponse.metadata` now returns the persisted message metadata
verbatim so live attached-doc bubbles carry provenance + the receipt exactly
like a reload (previously dropped at the Tauri boundary). Complex-task keeps
TaskProgress for the wait but its persisted `grounding_gate` meta feeds the
same receipt. Other non-streaming surfaces pass `None` and are byte-identical. Gated surfaces today (all env-gated;
`SOVEREIGN_GROUNDING_GATE` global default, `SOVEREIGN_GROUNDING_GATE_<SURFACE>`
override): streaming/non-streaming KnowledgeQuery + streaming DeepQuery
(dual-bank validated), attached-doc (dual-bank validated: Conrad dev bank +
Meridian holdout under `bench/attached_doc/`; `AttachedAssetSearcher` seals
claim search to the asset), complex-task (narration gated per-claim against
the step transcript, verbatim derivation appendix untouched; calibration bank
pending), simple-query (non-witness, retrieval-matched turns only). The
refinement overwrite path re-gates verify-only (`RefinementGuard`): a
gate-released answer is never overwritten by text that fails the same gate.
Corpus-deictic questions ("the story", "this document") close the GK-caveat
exemption like entity anchoring does. Metalingual answers are structurally
grounded instead of gated: decode-committed term-absent caveat / source
attribution + the quote guardrail (calibration bank pending). (Note: effort-tier escalation default-ON *improves* chaos
competence 0.33 → 0.46 — a net win, not a regression.)

**Retrieval pipeline (`runtime/retrieval_pipeline.rs`).** The
retrieval-injection orchestration — which grounding/boost/expansion steps run,
in what order, under which `SOVEREIGN_*` gates — is **data**: a
`RetrievalPipeline` is an ordered list of named `RetrievalStep`s run by one
tracing runner (one `tracing::info!(target: "retrieval.pipeline")` line per
step with `chunks_before/after/delta`). The governing principle: **the
intent decides HOW to answer (model tier, expansion, synthesis shape) — never
WHERE knowledge lives.** Both pipelines are composed as **the SHARED 3-step
evidence-gathering head (local corpora ∥ mesh fan-out → personal-scope filter
→ StateStore corpus docs) + the SHARED 15-step core + a per-intent tail**:
`kq_pipeline()` (KnowledgeQuery / ComparisonQuery, 19 steps; tail = audited
truncate, then route-aware expansion post-pipeline) and `deep_pipeline()`
(DeepQuery / SimpleQuery, 20 steps; tail = plain truncate + strategy-driven
top-sources expansion; attached-doc turns drop the head and the two grounding
steps). Golden tests pin the step lists and the head+core identity, so
reordering is an explicit, reviewed act. Per-intent differences
(comparison-aware entity boost/reserve) ride `PipelineState`, not divergent
code, and both pipelines share `shared_head_steps()` — which knowledge
sources exist is a property of the install, not of the intent label. The
injection helpers themselves (`apply_atlas_grounding`,
`apply_raptor_grounding`, `meta_atlas_boost`, `fan_out_decomposed_queries`,
`expand_from_top_sources`, …) are unchanged `impl Runtime` methods under
`runtime/retrieval/` (the 2026-07-12 split of the former 5,000-line
`retrieval.rs` into 11 concern modules); both handlers build a
`PipelineState` and call
`pipeline.run(...)`, then keep their post-pipeline concerns (evidence-shape
routing + route-aware expansion + prompt/request assembly on the KQ side;
provenance + prompt/history assembly + seal audit on the deep side).
Cross-path SSOTs hold on both paths: deep's expansion decision goes through
the same `decide_expansion_strategy` the KQ planner uses, the personal-scope
filter is one shared whole-pool step, and the store-search leg reuses the
pipeline's query embedding. `retrieval_pipeline_flags()` is the SSOT
registry of every retrieval env knob (name + default + purpose). (How the
two pipelines converged from silently-drifting inline duplicates — the
Phase 2 A/B, the divergence-archaeology pass, the accretion artifacts it
retired:
[HISTORY](./HISTORY.md#retrieval-pipeline-convergence-2026-06-09--10).) Environments without a mesh or
store-ingested corpora see identical behavior; the known mesh round-trip of
local corpora is collapsed by the shared `dedupe_merged` step. Open
follow-up: KQ provenance doesn't yet surface mesh peer attribution
(`search_method` labels live on the deep handler).

Per-intent handlers live in
`sovereign-core/src/runtime/handlers/{simple,ask_move,conation,commissive,metalingual,expressive,document_op,complex_task,attached_doc,knowledge_query,code_query,generative,recipe_author}.rs`
as `impl Runtime` across files (no vtable hop on dispatch).

### Inference

`sovereign-inference/src/embedded/` wraps `llama-cpp` with a
lazy-loaded slot system (Quick / Main / Code / Embed). Hybrid +
remote providers wrap OpenAI-compatible servers (vLLM, Ollama,
llama.cpp, TGI, cmnwlth). Full detail — slots, polished slot
management, sibling pool, decode paths, MTP, OICP scoring, harness
adapters, cutoff legibility, conversation-history compaction — in
[`docs/inference.md`](./docs/inference.md).

**Compute-slot process boundary (`sovereign-compute`, P1).** Optional
(`[compute] enabled`, default OFF): the daemon runs a slot's compute in a
supervised **child process** so a ggml `SIGABRT` kills only the child — the
daemon keeps gossip / `/status` / the client API — and observes the exit as an
event it re-plans around. **Its value is crash isolation + the can't-fit-one-box
(distributed 122B) case, NOT throughput.** A live embed run confirmed that for a
model that fits one box, N process replicas *lose* to in-process multi-sequence
batching (one batched kernel vs N processes thrashing one device, plus HTTP hop
+ weight duplication + thread oversubscription) — so the replica-pool path is a
demonstrated dead end for parallelism; the right lever there is extending
in-process continuous batching (FastShort-style) to the primary + streaming.
A child is `current_exe() --compute-child` (no new artifact); it speaks a
**native lossless wire** (`POST /internal/complete[_stream]` carrying serde
`CompletionRequest`/`StreamFrame` verbatim — grammar/allowlists/sampling_mode
survive, llguidance runs in-child). Daemon-side: `ComputeRoutedProvider` routes
by `model_id` to the child for that slot, else the in-process engine;
`ComputeChildManager` supervises one child per `[[compute.slot]]` and streams
lifecycle to `/status` (`compute_children`, target `compute_child`). The child
supervisor was extracted here from the desktop (shared, byte-identical).
Crash-isolation acceptance is proven (`compute_child_e2e.rs`). See
`docs/DISTRIBUTED_PILOT_READINESS.md` P1.

**The distributed primary in a child (`[compute] distributed_primary`, default
OFF).** The payoff the boundary was built for. Distributing a primary across
mesh workers puts ggml's error-path-free RPC client inside the daemon: a worker
that dies mid-decode (`ggml-rpc.cpp:491`), or one already gone when the
prune-reload frees its buffers (`:386` — this killed the daemon live on
2026-07-27, from the shrink-fast-prune path meant to protect it), SIGABRTs the
whole process. In this mode the daemon **withholds the primary entirely**
(`primary_path: None`, so no in-process path can lazily load it) and a
`DynamicChildSlot` owns it instead. The division of labour is forced by reach:
only the daemon can warm (the orchestrator needs the mesh member directory + the
iroh transport), only the child should load. So the daemon plans + warms via
`warm_distributed_primary` — the extracted shared tail of
`resolve_placement_inner`, one code path for both — writes a
`DistributionHandoff {endpoints, plan}` JSON, and spawns a child with
`SOVEREIGN_RPC_ASSUME_WARMED=1` that pins that plan (`pin_shard_plan`) before
loading via `EmbeddedLlamaCpp::load_single_distributed`. Shipping the *plan*, not
just the worker list, is what extends the plan-agreement invariant across the
process boundary: the shard cache is process-local, and a child that re-planned
against post-warm VRAM would cut the blocks differently, miss every warm cache,
and fall back to bulk weight send. Worker-set changes are **kill + respawn**, not
reload — the discovery loop respawns the child instead of calling
`reload_primary()`. `ComputeRoutedProvider` claims primary-class traffic (named
primary, or unnamed at `Speed::Slow`/`Medium`; never unnamed `Speed::Fast`, which
the in-process fast slot still owns) and never falls back to `inner` for it —
the answer while the cluster re-forms is a fail-fast `ComputeUnavailable` → mesh
cascade → clean 503. `/status` shows the primary as `mode: "child-distributed"`.
Respawn acceptance: `distributed_primary_respawn_e2e.rs`.

**CPU-arch compatibility gate + crash capture (desktop).** Recurrent /
linear-attention architectures — Qwen3.5 "Gated DeltaNet" (`qwen35`),
Mamba/SSM, RWKV — drive an out-of-bounds write in ggml's recurrent
`ggml_compute_forward_set` during **CPU** prefill (an upstream llama.cpp bug;
disabling the fused chunked kernel does not help, and there is no toggle that
avoids it). They run fine on GPU. Two layers keep a user's first message from
hard-crashing the app:

- **Proactive substitution** — `sovereign-inference::gguf_meta::read_architecture`
  reads `general.architecture` straight from the GGUF header (zero weight load),
  and `cpu_compat::choose_cpu_safe_chat_model` decides `Keep` / `Substitute` /
  `NoSafeModel`. At desktop boot (`state/builders/model_compat.rs`, run before
  `inference::load_inference`) a CPU machine whose configured chat model is an
  unsafe arch gets a **dense** substitute discovered alongside it (largest
  non-embedder GGUF) and a non-fatal `model-notice` banner; with no substitute,
  boot fails with a clear in-app `backend-error` rather than a silent SIGSEGV.
  GPU machines are a no-op.
- **Backstop + capture** — the pre-load subprocess smoketest (`smoketest.rs`)
  still guards the GPU path; on a native crash it records a durable, submittable
  `CrashRecord` (`crash_report.rs` → `~/.sovereign/crashes/*.json`). A
  process-wide panic hook captures Rust panics the same way. Records are
  local-first and **never auto-uploaded**: the in-app Diagnostics surface lists /
  views / deletes them, and one-click `export_crash_record` writes a redacted
  markdown copy to the Desktop + hands back the GitHub Issues URL — mirroring the
  daemon-crash flow in `crash_bundle.rs`.

### Tools

| Tool                    | Purpose                                                      |
|-------------------------|--------------------------------------------------------------|
| `SearchTool`            | Local vector + FTS5, coverage assessment, optional web fallback |
| `WebSearchTool`         | NL → keywords (Quick), search backend, fetch top 3, synth w/ citations |
| `WebFetchTool`          | Single-URL fetch + HTML→text                                 |
| `KnowledgeTool`         | Direct corpus query                                          |
| `ClaimSearchTool` / `EpistemicLandscapeTool` | Enriched-corpus retrieval         |
| `DocumentTool`          | Map-reduce summarize/analyze (4 chunks/batch, 8K reduce)     |
| `ShellTool` / `FileTool` / `EmailTool` / `CalendarTool` / `ComputeTool` | Standard tools (sandbox + approval) |
| `McpClient` + `McpToolAdapter` | stdio JSON-RPC + HTTP+SSE; wrap remote MCP servers as native tools |

**External MCP servers (client direction).** HTTP MCP servers are configured in
the `[[mcp_servers]]` array of `~/.sovereign/config.toml`
(`SetupConfig.mcp_servers`) — added via `svrn mcp add` or **Settings →
MCP** — and loaded into the agent's tool registry at startup by the one shared
loader `sovereign_tools::mcp::load_from_setup_config`, which **every** chat
surface calls (`svrn chat`, the desktop bootstrap, `svrn serve`).
Each MCP tool's descriptor is enriched (`McpToolAdapter` synthesizes an example
call from the input schema + passes through any `outputSchema`) so the planner
reliably emits a tool step instead of a reason step; tools declare
`Permission::Network`, so the executor's approval gate fires on first use
(add-time trust on the auto-approving CLI). `McpToolAdapter` also infers each
tool's `effect`/`idempotency` from its name (`infer_behaviour`) — read verbs
(`get_`/`list_`/`snapshot`/`navigate`/…) → Read/Idempotent, mutation verbs
(`create_`/`click`/`type`/`submit`/…) → Write/NonIdempotent — so a browser
`click` that submits a form picks up the approval gate + replay ledger while a
`snapshot` read does not. Driving a real browser via `@playwright/mcp` (the
first heterogeneous-app actuator) is a runbook:
[`docs/BROWSER_ACTUATOR.md`](./docs/BROWSER_ACTUATOR.md), proven live by
`sovereign-tools/tests/playwright_actuator.rs`. The config DTO lives in
`sovereign-core::mcp_config` (so `SetupConfig` can carry it without a crate
cycle) and is re-exported from `sovereign_tools::mcp`. `svrn mcp
demo-server` runs a sealed-fact reference server
(`sovereign-cli-llm/src/mcp_demo_server.rs`) for an end-to-end demo: a tool
whose output exists nowhere else, so a correct answer in chat proves the model
actually called it.

**Attach-a-file-for-tools (desktop).** Vision / audio MCP tools take a file
*path* (the model stays text-only — the tool does the modality work). The
desktop's media-attach (image/audio) binds a file's absolute path to the turn
and prepends a `▸ attached file: … path: …` block to the message before the
runtime sees it — the same "augmented message" rail `context_chunks` uses
(`commands/chat.rs::build_tool_files_preamble`), so the model passes the path to
e.g. `describe_image(path)` / `transcribe_audio(path)` on a *local* MCP server
with no Runtime change. Distinct from a *document* attachment (which is ingested
for RAG and discards the path). Spec: `docs/specs/ATTACH_FILE_FOR_TOOLS.md`
(P1 shipped; P2 threads a typed `ToolContext.attached_files`).

**Code-intelligence MCP server**. Long-running variant via
`svrn daemon`; ad-hoc via `svrn project serve`. Tools
under `sovereign-tools/src/code/` cover code index (`symbols`
= `symbol_lookup`, `code_search`, `recent_changes`, `working_set`,
`brief`), the session-orientation brief as an MCP tool (`briefing` —
the SessionStart hook's renderer callable by any MCP client; the
daemon variant threads its live work-atlas store so the brief's
"Work in flight" section shows peer claims/edit-observations
overlapping the working set), the deterministic tree-sitter fact base (`facts` — fn defs
/ config construction-fields / string literals, cited + freshness-
stamped, embed-free; see [`docs/CHECK_CODE_AGAINST_SPEC.md`](../docs/CHECK_CODE_AGAINST_SPEC.md)),
SCIP call graph (`callers`, `callees`, `blast_radius`),
lint/test watchers (`lint_status`, `get_lint_output`,
`test_status`, `run_tests`, `get_run_output`, `build`), notes
(`write_note`, `read_notes`, `delete_note`, `suggest_note`,
`promote_note`, `read_note_by_id`, `read_note_digest`), ATOS
feature lifecycle (`provision_feature`, `archive_feature`,
`record_atos_event`, `write_redteam_finding`, `atos_plan_emit`,
`atos_utils`, `atos_verify`, `spec`), drift (`drift`,
`drift_posture`, `drift_findings`), capability docs
(`capability_map`, `capability_posture`, `capability_findings`),
project + design context
(`project_context`, `design_signals_extract`, `check_doc_paths`,
`index_health`), session reflection (`session_reflection`), and
work-atlas coordination (`declare_scope`, `release_scope`,
`work_in_flight` — see [`docs/WORK_ATLAS.md`](./docs/WORK_ATLAS.md)).

**Live graph freshness (`sovereign-mesh::reindexer`).** The daemon's
tool graph and the reindexer share ONE merged `ScipGraph` handle
(built once in `daemon_cmd/mod.rs`, passed to both `build_tool_registry`
and `start_freshness_pipeline`), so reindexer updates are visible to
`symbols`/`callers`/`blast` live, without a daemon restart — before
this unification the tools read a frozen startup snapshot. On each
debounced save the reindexer runs a **tree-sitter overlay**
(`facts::extract_symbol_defs` → `ScipGraph::replace_file_symbols_for`):
embed-free, no rust-analyzer, symbol *defs* fresh in milliseconds and
never contending with inference. The heavy whole-workspace
rust-analyzer export is **demoted** — spawned (never blocking the watch
loop), rate-limited to at most once per `FULL_REBUILD_COOLDOWN`
(300s) of active editing plus on git-HEAD (commit), and **quiescence-gated**
(2026-07-24): an FS-due export waits for `FULL_REBUILD_QUIESCENCE` (30s)
of no saves before launching (capped by `FULL_REBUILD_MAX_DEFER`, 600s,
so continuous editing can't starve it; commit/explicit rebuilds are not
gated). The exporter subprocess itself runs `nice +10`
(`scip_export.rs` pre_exec) so a multi-minute pass yields to interactive
work. So it no longer fires on every save (the contention that had the
watcher disabled).
Cross-file call edges and qualified names therefore lag one full export
(accepted eventual-consistency); overlay rows carry `qualified_name=""`,
`kind="function"`. Staleness levels still carry calibrated confidence:
`None` / `SomeCallSitesMayBeStale` / `GraphIsAging` / `GraphIsStale`
/ `LanguageNotIndexed`. `blast_radius` does BFS over the call graph
and appends a `macro_hints` text scan for references SCIP doesn't
capture. Index posture is surfaced in-band (2026-07-24): `symbols`
appends the same `IndexHealth` trailer `callers`/`callees`/`blast`
carry, and `code_search` appends a chunk-index posture note — absent /
degraded (unreadable) / aging (`IndexInfo.last_updated` ≥7 days) — so a
stale chunk index can no longer masquerade as "no matches";
`agent-preflight.py` checks the same stamp for the corpora in
`quality/agent-preflight.golden.json::code_corpora`.

**One project owns one workspace (nested-root guard).** Project
registration (`POST /v1/projects/register`, used by both `project
register` and `project init`) refuses a root that is an ancestor or
descendant of an already-registered project's root
(`Registry::nested_conflict`, 409 with the conflicting corpus named;
`--force` overrides). Nested registrations are how the freshness
pipeline collapses: every save inside the shared subtree dirties all
overlapping projects, each queues its own full-workspace
rust-analyzer export on the single global rebuild permit, and the
queue never drains (observed 2026-07-23: four nested projects —
monorepo root + three subtree projects — all permanently
`[rebuilding]`, one never built). The monorepo is one cargo workspace,
so subtree projects buy no smaller export anyway. Canonical
registration for this repo: the single `commonwealth-ai` project at
the repo root.

**Capability docs (derived architecture).** A pipeline that derives
*what the codebase does* from the SCIP call graph and reconciles it
against the prose docs. `code capability-map` clusters entry points
that share a reachable call spine into capabilities (226 on this repo,
language-agnostic core + an entry-point seam); `enrich capability-doc`
narrates each capability from cached `enrich code-intel` summaries into
grounded prose, every spine function cited `file:line`; `enrich
capability-reconcile` matches capabilities against the architecture
docs → **corroborated / undocumented / drifted** findings (deterministic
ident-match → meaning-based LLM verify → a precision-biased drift judge —
drift ships biased hard toward precision, since one phantom contradiction
destroys trust). Artifacts land in `~/.sovereign/capabilities/<corpus>/`
(`capability_map` / `capability_doc` / `capability_findings`.{md,json}
plus a `.fingerprint`); `capability_posture` and `capability_findings`
are the freshness-gated read tools, siblings to `drift_*`. This is
"drift to the next level": the drift system reconciles *names*, this
reconciles *capabilities* — does the code do what the doc claims. The
once-planned next phase (symmetric `spec-intel` → a spec↔code bipartite
diff) shipped as the spec↔code fact pipeline — `svrn code facts` /
`enrich spec-intel` / `code check-spec`; see
[`../docs/CHECK_CODE_AGAINST_SPEC.md`](../docs/CHECK_CODE_AGAINST_SPEC.md).
The deterministic floor of this stack runs in public CI:
`cargo run -p xtask -- docs-gate` (workflow `docs-reconcile.yml`,
badged on the README)
resolves every repo path THIS document and ARCH_PRINCIPLES cite —
machine-local citations and paths that no longer exist fail the build.
The LLM-bound layers above it (drift, capability-reconcile, check-spec)
stay mesh-side.

### State, memory, skills

- **State**: `sovereign-store` provides `SqliteStateStore` (default),
  `PostgresStateStore` (deadpool + tokio-postgres),
  `MemoryStateStore` (tests). One trait, three impls. Schema in
  `migrations.rs`: `conversations` + `messages` (FTS5), `tasks`,
  `memories`, `documents`, `corpus_states`, `routing_log`,
  `search_budget`, `permissions`. Every record carries a Lamport
  `version`; soft-deletable rows have `deleted_at`. Two stores can
  union-merge without schema migration.
- **Working memory** — compressed every message via
  `memory::compress_working_memory` (Quick slot, ≤200 tokens) into
  `{ current_goal, facts, active_documents }`.
- **Conversation memory — three constant-capacity channels, now
  visible.** What a long thread carries forward is (1) the rolling
  visible window, (2) the **conversation frame** (`conv_frame.rs` — five
  named sections Topics / Entities / Stated goals / Commitments / Open
  threads, 320-token budget, persisted in `conversations.frame`, folded
  incrementally by `maybe_compact_dropped_history` off a watermark
  carried in the document's own frontmatter), and (3)
  **retrieval-over-history** (`maybe_retrieve_relevant_history` — the
  dropped turn-pairs embedded once and memoized per `Runtime`, hybrid
  cosine+entity scored, MMR-selected). Total is ~2.8k tokens
  *independent of conversation length*. The frame replaced a re-narrated
  prose blob for two reasons, only one of which is cost: a blob has to be
  rewritten to be updated (and re-narration is where named entities get
  dropped), and a blob is not **renderable** — "what do you remember
  about this conversation?" is answered from sections, and the
  metalingual Conversation branch now does exactly that, putting the
  frame in the prompt ahead of the verbatim turns. Both channels narrate
  themselves: `NarrationPhase::ConversationRecall` (memory being read —
  turn indices + best similarity) and `ConversationFolded` (memory being
  written). Recall also lands in `TurnProvenance.history_recall` so the
  ledger can tell a verified recall from a lucky parametric guess; the
  chip is why the streaming surface runs retrieval-over-history *after*
  `sessions.begin` rather than before.
- **Long-term memory** — extracted at conversation end. Each
  `Memory` has `confidence`, `created_at`, `last_used`. FTS5
  retrieval. Exponential monthly decay; pruned below
  `prune_threshold`.
- **Tiered memory retrieval** (spec
  `docs/specs/TIERED_RETRIEVAL_MEMORIES.md`) — the embed-recall path
  (`memory::recall_relevant_memories_embed`, relational/witness
  surfaces) reads **persistent T1 embeddings**
  (`memories.embedding + embedding_model`, computed on write at
  `save_with_contradiction_check`/compaction, lazily backfilled on
  first recall; `embedding_model` must equal the provider's
  `embed_model_id()` or the row re-embeds — the model-swap guard) and
  blends a **T3 memory-RAPTOR** signal: per-scope trees in
  `mem_raptor_nodes` (batch builder
  `sovereign-tools::mem_atlas::build_memory_atlas`, journal-tuned
  leaf clusters of ~7; incremental maintenance
  `sovereign-tools::mem_tree::insert_memory` — MemTree-style descent
  + a 4-op trigger ladder attach/re-summarize/split/rebuild with
  BIRCH-CF + Page-Hinkley gates, every trigger emitting a glassbox
  `InsertTrace`). Level-0 node matches lift member leaves by
  `max(leaf, α·node + (1−α)·leaf)`. Scope key = the memory wall
  (`MemoryScope::atlas_key()`), so a node never summarizes across
  scopes. Production trigger: the knowledge-view debouncer's
  `MemoryTouched` window drains touched ids through `insert_memory`
  (handles installed via
  `KnowledgeViewManager::install_memory_atlas`). Bench:
  `svrn eval inner-chaos --recall-probe / --recall / --recall-stream`.
  Live-path invariant (2026-07-10): the embed-recall stanzas in
  `handle_turn`/`handle_message_stream` run PRE-ROUTING, where
  `context.turn_register()` still returns the Factual fallback — they
  gate on mode-derived relational-ness (`resolve_active_mode`), never
  on `turn_register()`. On witness turns the recall result then passes
  through `merge_recall_pins` (reference-driven sticky pins: the entry
  a grounded reply actually spoke about, attributed by the grounding
  verifier's `referenced` field, stays in view ≤2 pins / 5-window);
  glassbox via `RUST_LOG=memory_grounding=info` (gate verdict, pin
  set, pin merge) and `TurnProvenance.recalled_memories`, captured on
  BOTH expressive variants (the recall bench judges against this
  actual window, not a retrieval replica).
- **Routing-correction memory** —
  `RoutingCorrection { message_hash, classified_as, was_correct }`
  fed back into the router prompt as "avoid these mistakes."
- **Skills** — TOML files under `sovereign/modes/` (current:
  `recipe-author`, `inner-work`). `SkillRegistry` merges routing
  hints, planner templates, prompt overrides, memory rules, and
  OICP requirements into the runtime. Skills carry
  `signature` / `signed_by` and a derived `TrustLevel
  { CommunityReviewed, AuthorSigned, Unsigned }`.

### Frontends

| Frontend            | Purpose                                                                              |
|---------------------|--------------------------------------------------------------------------------------|
| `sovereign-cli` (+ siblings) | User-facing dispatcher. `sovereign <verb>` execs into one of three siblings — `sovereign-cli-daemon`, `sovereign-cli-dev`, `sovereign-cli-llm` — based on the verb. Same UX as one binary; faster builds. Discovery: each sibling at `current_exe()`'s parent dir; override via `SOVEREIGN_CLI_{DAEMON,DEV,LLM}_BIN`. Unix execs into the sibling (same PID); other platforms spawn-and-wait. |
| `sovereign-server`  | Axum REST + WebSocket on configurable port; multi-tenant via `tenant.rs` with per-tenant isolation on corpora and uploaded documents (`ConversationContext.corpus_ceiling` scopes retrieval incl. the round-0 engine search; `DocumentAsset.owner` gates document list/get/delete/ask — the SaaS-hub hardening, 2026-07); server-side `ApprovalChannel` w/ `/v1/tasks/{id}/approve`. **Mobile-facing surface** (`docs/specs/MOBILE.md`): WS `/v1/conversations/{id}/stream` streams `ServerEvent::Token`→`Complete` token-by-token down the requesting socket (not the shared broadcast — avoids cross-tenant leak); `projection.rs` surfaces typed `provenance` + `citations` on REST message responses; `GET /v1/corpora` lists `CORPUS_REF`s (Knowledge-only, with `scope`/`mesh_shared` privacy posture derived from `IndexInfo.mesh_sharing`); a `scheduler.rs` `FairScheduler` bounds concurrent turns — a weighted-fair queue + per-origin cap with live `ServerEvent::QueuePosition` over WS and `503 + Retry-After` shed (`busy.rs`) on REST, sharing its `commonwealth_core::fair_sched::SchedCore` policy core with the mesh peer-admission gate (so both are fair by identical rules); reciprocity weights from the contribution ledger rank a contributor's turns up. Secure by default: binds `127.0.0.1:8080`, and a non-loopback bind with `[auth]` disabled is refused at startup (`config::validate_exposure`; explicit `allow_unauthenticated_remote` opt-out) — permissive CORS is applied only when auth is on (`[server] cors = "auto"`). |
| `sovereign-desktop` | Tauri 2 + Svelte 5. The **UX-refactor (P0–P4) reshaped the app around user intent** — rail `Ask · Library · Reflect · Workshop · ⚙`. **Ask** (the branded chat w/ streaming + provenance) is the landing. **Library** (`library/{LibraryView,AddSheet,NotebookDetail}` off the `notebook_list` command) is the knowledge home — a notebook shelf with per-notebook Ask + Explore; the catalog `KnowledgeStatus` + folder/vault/import ingest fold into Library→Add; the Atlas rail is gone (the atlas surface lives inside a notebook's Explore via `AtlasSurface startingCorpusId` + as a reading deep-link target). **Workshop** (`workshop/WorkshopView`) holds the maker facets Build · Run · Test · Connect tools (MCP) · Open to apps (OpenAI endpoint), with a notebook→Workshop "use→make" bridge. **Settings** shrank to General + Operator (Mesh · Sharing · Mobile) clusters. A follow-on **elegance pass** layered craft on top: a plain-language scope bar in Ask (`AskScopeBar` — "Asking ‹notebook›", gating `CorpusFilterStrip`), per-notebook **conversation memory** (the `notebook_conversations` command → `SqliteStateStore::list_conversations_for_corpus`, a `json_each` filter on `enabled_corpora`; a notebook's Ask resumes its last thread, switched via a **Conversations ▾** dropdown), a card→detail **shared-element morph** (`lib/motion.ts` `crossfade`), and an **Ask↔Explore** Map→Ask bridge ("Ask about this" on an atom → the notebook's Ask, seeded). The per-notebook detail consolidates its chrome into **one header bar** — segmented `Ask | Explore` + a `⋯` overflow for Sources/Settings — with the scope stated by the header (the in-notebook scope bar suppressed via `ChatView hideScope`); the **Home hub was dropped** so the branded Ask flow is the first-run landing. **Layout is token-driven, not per-component.** `app.css` owns a layout scale (`--gutter` / `--gutter-top` / `--gutter-bottom` / `--measure` / `--measure-prose`) plus three global primitives — **`.page-body`** (the scroll container + gutter every surface body needs), **`.page-measure`** (the centred content column), **`.page-header`** (a header band on the same gutter). These are global rather than Svelte-scoped on purpose: the app's surface hosts (`.library-surface`, `.settings-surface`, `.nb-body`, `.app-chrome-content`) are all `height:100%; overflow:hidden` clipping boxes, so **a body that fails to establish its own scroller is clipped with no way to reach the content past the fold**. A July 2026 audit found exactly that — `ConflictsPanel` hid 2,442px of governance decisions behind an `overflow-y:auto` that could never fire (it sat on an auto-height box), and `AddSheet`'s body rendered flush to both window edges because a `padding:0` "embedded" opt-out outlived the host that used to compensate for it. `tests/e2e/specs/library-layout-audit.spec.ts` is the regression gate: it drives every Library route, measures composited geometry, and fails on unreachable content or a body inside the gutter. Do **not** re-declare padding/overflow on an element carrying `.page-body` — Svelte scoping gives the local rule higher specificity and it wins silently. Plus skill manager, `sovereign://` deep-link handler, system tray; reuses the shared `@sovereign/chat-ui` package (`packages/chat-ui`). |
| `sovereign-mobile` (`/sovereign-mobile`) | Thin Tauri 2 client (iOS + Android) — **no local inference/Runtime/corpus**. Reaches a host's `sovereign-server` over the tailnet, authenticates as a tenant (token in keychain), renders streamed chat. Rust core owns transport (HTTP + WS), SQLite cache of the spec's cached projections, and a fail-closed connectivity monitor; re-emits the SAME `message-chunk`/`message-complete` events the shared chat FSM consumes. Conversations are cached for display and referenced as a conversation `CORPUS_REF` once host-indexed (`indexed_in_corpus`); long-context is host-side (phone sends only the new turn + conversation id, never re-uploads history or embeds); local-only sources are privacy-badged (`scope`/`mesh_shared`). Detached from the Cargo workspace (own `[workspace]`); Rust core + Svelte UI are written but never compiled — pending the Tauri-mobile-toolchain pass on a Mac (`/sovereign-mobile/HANDOFF.md`). See `docs/specs/MOBILE.md`. |

Verbs by sibling binary:

- `sovereign-cli` (dispatcher + light delegators, no LLM dep) —
  `notes`, `status`, `drift`, `audit`, `cache-audit`, `session`,
  `charter`, `amend`, `design`, `plan`, `init`, `milestone`,
  `refresh`, `reflect`, `rough-edges`, `archaeology-eval`,
  `git-archaeology`, `agent-bench`, `nudge`, `serve`, `stop`,
  `memory`, `awareness` (feature-gated). `session`
  (`session_cmd.rs`) is the session-continuity surface: `session
  distill <id>` parses a Claude Code transcript (same source as
  `cache-audit`), extracts the deterministic narrative spine, and
  synthesizes a schema-v1 session frame via one daemon chat call —
  see `sovereign/docs/specs/SESSION_CONTINUITY.md` and the graded
  golden at `quality/session-frame.golden.md`. Frames + spines land
  under `~/.sovereign/sessions/<session_id>/`. `session frames`
  is the read side: the INDEX of live frames in selection order
  (branch match → prompt overlap → recency), with `session frames
  <id>` dereferencing one whole. Both are pure filesystem reads, so
  the handoff survives a dead daemon; `.claude/hooks/session-boot.sh`
  injects the index at SessionStart and
  `.claude/hooks/inject-notes.sh` injects the selected frame on the
  first prompt (MEMORY_MODEL §5 E5 Phase 2). The initiative-level
  design compass for all of this (context = working memory holding
  pointers/gists; notes/frames/facts/code-graph = external long-term
  store; eviction and forgetting policies) is
  `docs/specs/MEMORY_MODEL.md` — `cache-audit --counterfactual`
  prices its levers H1–H5.
- `sovereign-cli-daemon` — `daemon` (owns :9741), `setup`,
  `install-service`, `doctor`.
- `sovereign-cli-dev` — `atos`, `project`, `code`, `tools`.
- `sovereign-cli-llm` — `chat`, `bench`, `eval`, `voice`,
  `reading-diag`, `atlas`, `meta-atlas`, `enrich`, `recipe`,
  `recipe-agent`, `maintainer`, `pipeline`, `mcp`, `alignment`,
  `mesh`, `meshapp`, `mobile`, `corpus`, `newsworthy`,
  `knowledge-gym`, `search-gym`, `govern`, `router-cache`,
  `proxy`, `portfolio`, `workflow`, `claim`.

The dispatcher's `ALL_VERBS` const (test-pinned:
`all_verbs_is_complete_and_sorted`) is the SSOT this list mirrors.

There is no interactive REPL. Bare `sovereign` prints usage and
exits; use `svrn chat` for the interactive shell, which
streams through the daemon's `/v1/chat/completions`. `project init`
prompts for AI-assistant harness (Claude Code / opencode / both /
skip) and writes `.opencode/opencode.json` + `AGENTS.md` and installs
the ATOS opencode plugin.

The daemon (`sovereign-cli-daemon::daemon_cmd::run`) rotates its
own logs at startup via its `log_rotation.rs` — copy-truncate, 10
MiB cap, 5 backups, 30-min sweep loop; preserves the inode for
launchd-held FDs.

### Deep-link handler

`sovereign-mesh/deep_link.rs` parses `sovereign://create?name=<name>`
and `sovereign://join?key=<key>` (with relay hints for NAT
traversal). The desktop app registers as the system handler.

### Subsystems with their own docs

| Subsystem | Doc |
|---|---|
| Slots, OICP, harness, cutoffs | [`docs/inference.md`](./docs/inference.md) |
| Inline completion (FIM) — ghost text served by the daemon: `[models.fim]` opt-in (lean alias OR pinned dedicated slot), vocab-probe marker detection, `POST /v1/completions`, stop-craft tracker, VSCode extension, measured latency. **Onboarding is one command: `svrn setup --fim`** (`setup_cmd/fim.rs`) — plan-then-consent, downloads Mellum2 off the `[profiles.fim_*]` ladder in `models.toml`, writes lean-mode config (`primary` == `models.fim.path`, one resident copy), starts/restarts the daemon, then walks the same three probes as the extension's Diagnose command (reachable → `inference.fim` non-null → real completion round-trip) before installing the `.vsix`. Mellum2-only by operator decision; `--quant` moves rungs. A dedicated FIM slot beside a separate chat primary is deliberately NOT offered — smallest Mellum2 is 7.0 GB against ~3.5 GB of headroom on the `high`/`very_high` tiers | [`docs/INLINE_COMPLETION.md`](./docs/INLINE_COMPLETION.md), [`../packages/vscode-sovereign/README.md`](../packages/vscode-sovereign/README.md) |
| Next-edit prediction — after ≥2 repeated edits, the editor proposes the remaining sites as a tab-through diff queue. Two lanes behind one route (`POST /v1/edit_predictions`): the **rule lane** (pure induction in `commonwealth-api/src/next_edit.rs` — context-expanded literal rules, structural-confidence threshold, no inference) and the **model lane** (`next_edit_model.rs` — deterministic consult gate, needle-anchored region rewrite on the resident FIM slot), both SHIPPED and eval-gated green, model lane default-on. Casing-variant renames are detected but declined pending a deterministic sub-lane. The extension coalesces keystrokes into edit units (`packages/vscode-sovereign/src/editUnits.ts`) and renders under a never-scroll-uninvited policy; all policy is daemon-side so IDE clients stay thin. Hardened 2026-07-30 (§9a): permit outlives the inference (a dropped future cancels nothing), region bounded in bytes, drop-never-repair on model output, one shared byte-ruler for the wire contract. Glassbox: `sovereign_debug` explains silence per response; `next_edit` tracing target. User guide: [`docs/NEXT_EDIT_IN_YOUR_EDITOR.md`](../docs/NEXT_EDIT_IN_YOUR_EDITOR.md) | [`docs/NEXT_EDIT.md`](./docs/NEXT_EDIT.md) |
| Glassbox reading surface + Atlas Inspector | [`docs/knowledge-view.md`](./docs/knowledge-view.md) and `sovereign-tools/src/atlas_view/` |
| **Collection notebooks** — Explore as an article picker | A corpus ingested as ONE index but enriched **per article**: SEP's 182k paragraphs live in `sep`, its map lives in ~1,769 sibling `sep-<slug>` atlases (`sovereign-recipes/sep/recipe.toml` `[enrichment]`). The parent's own `atoms.json` is a 44-byte `{"atoms":[]}`, so the ordinary atom browser had nothing to show. `FileAtlasReader::list_members` enumerates the prefixed members that carry a non-empty atlas → `atlas_list_members` → `AtlasCollectionView.svelte`, which lists the articles; picking one opens **its** atlas in the ordinary `AtlasCorpusView`. `AtlasSurface` routes on `CorpusKind = atom \| conv \| collection`, resolving "collection" from a non-empty member list. Member titles are **slug-derived** (`sep-logic-modal` → "Logic Modal") because nothing on disk carries the upstream title. Explorability is gated on **atom count**, never on the presence of an `atlas/` dir — that distinction is what kept SEP's Explore tab from claiming a map it did not have. |
| KnowledgeView landscape splice | [`docs/knowledge-view.md`](./docs/knowledge-view.md) |
| ATOS — agent task orchestration | [`docs/ATOS.md`](./docs/ATOS.md), [`docs/ATOS_RUNNER.md`](./docs/ATOS_RUNNER.md) |
| Architectural-correctness tooling | [`docs/DRIFT_DETECTION.md`](./docs/DRIFT_DETECTION.md), [`docs/CORRECTNESS_TOOLING.md`](./docs/CORRECTNESS_TOOLING.md), [`docs/GIT_ARCHAEOLOGY.md`](./docs/GIT_ARCHAEOLOGY.md), [`docs/ARCHAEOLOGY_EVAL.md`](./docs/ARCHAEOLOGY_EVAL.md), [`docs/PLAN_ALIGNMENT.md`](./docs/PLAN_ALIGNMENT.md) |
| Knowledge bases + tiered retrieval | [`docs/KNOWLEDGE_BASES.md`](./docs/KNOWLEDGE_BASES.md), [`docs/TIERED_RETRIEVAL.md`](./docs/TIERED_RETRIEVAL.md) |
| Retrieval redesign — component model, measured integrity findings, phased swings | [`docs/RETRIEVAL_REDESIGN.md`](./docs/RETRIEVAL_REDESIGN.md) |
| Epistemic state — the answer as a typed object (per-claim provenance, gap conjecture, acquisition routes; SHIPPED — ledger on every answer surface, typed chaos verdict primary, gap.rs's LLM judge deleted 2026-07-20 in favor of the gate's abstention signal, status in `docs/EPISTEMIC_STATE_STATUS.md`) | [`docs/EPISTEMIC_STATE.md`](./docs/EPISTEMIC_STATE.md) |
| Work-atlas peer coordination | [`docs/WORK_ATLAS.md`](./docs/WORK_ATLAS.md) |
| Product demo reel as an acceptance suite — 9 beats driving real surfaces against the operator's live daemon; a beat that fails its assertions exports no clip (`npm run demo` → `npm run demo:export`) | [`crates/sovereign-desktop/tests/e2e/demo/DEMO_BEATS.md`](./crates/sovereign-desktop/tests/e2e/demo/DEMO_BEATS.md) |
| **Desktop quality surface — START HERE for "how do I verify the desktop".** Every gate in one map: the four commands that gate a merge, the three Playwright configs and which specs each owns, the flags that are load-bearing (`--fail-on-warnings`, `-c playwright.real.config.ts`, `--allow-empty`), the `SOVEREIGN_REAL_*` / bridge / supervisor env vars, the port invariant (`:9741` must be free or runs are invalid), and an explicit list of what CI does **not** run | [`crates/sovereign-desktop/QUALITY_SURFACE.md`](./crates/sovereign-desktop/QUALITY_SURFACE.md) |
| Negative controls — proving the desktop suite can fail. Every other measure (spec counts, invoke-coverage, fixture liveness) describes what the tests REACHED; this one breaks the product on purpose and requires the specs that claim the coverage to go red. Two layers: `specs/negative-controls.spec.ts` stages broken turns against the real-mode invariant pack (CI thereby guards a suite it never runs), and `npm run sabotage` applies declared source mutations and reports CAUGHT / SURVIVED / STALE. Gates in CI; a `SURVIVED` verdict is a bug report about the suite | [`crates/sovereign-desktop/tests/e2e/NEGATIVE_CONTROLS.md`](./crates/sovereign-desktop/tests/e2e/NEGATIVE_CONTROLS.md) |
| Browser actuation (MCP → Playwright) | [`docs/BROWSER_ACTUATOR.md`](./docs/BROWSER_ACTUATOR.md) |
| TDD machine | [`docs/TDD_MACHINE.md`](./docs/TDD_MACHINE.md), [`docs/TDD_MACHINE_DESIGN.md`](./docs/TDD_MACHINE_DESIGN.md) |
| Solver design | [`docs/SOLVER_DESIGN.md`](./docs/SOLVER_DESIGN.md) |
| Local corpora / Obsidian / watched folders | `sovereign-tools/src/local_corpus/` — invariants pinned via tests in that crate |
| Wikipedia freshness layer | `corpus-engine/src/update/newsworthy*.rs` + `sovereign-recipes/wikipedia-newsworthy/` |
| Per-document index recency (Atlas fresh-first) | `corpus-engine/src/freshness.rs` — source-agnostic `source_doc_id → unix` sidecar (`_doc_freshness.json`) stamped at the single reindex convergence point (`engine::reindex::reindex_by_source_doc_id`); `ChunkRef.source_doc_id` carries the join key onto atoms, and `sovereign-tools::atlas_view::atom_browse` sorts atoms fresh-first + sets `AtomSummary.updated_at`. ANY re-indexing source (newsworthy, watched-folder edit, delta) makes its content "fresh" with no per-source code — freshness is emergent from indexing. |
| Pinned worker pods as inference peers | [`docs/PINNED_WORKER_AS_INFERENCE_PEER.md`](./docs/PINNED_WORKER_AS_INFERENCE_PEER.md), [`docs/EPHEMERAL_WORKER_PODS.md`](./docs/EPHEMERAL_WORKER_PODS.md) |
| Cloud peer deploy | [`docs/CLOUD_PEER_DEPLOY.md`](./docs/CLOUD_PEER_DEPLOY.md) |
| Mesh load awareness | [`docs/MESH_LOAD_AWARENESS.md`](./docs/MESH_LOAD_AWARENESS.md) |
| Voice contract harness | `sovereign/bench/voice/README.md` |
| Production search integration | [`docs/specs/PRODUCTION_SEARCH_INTEGRATION.md`](./docs/specs/PRODUCTION_SEARCH_INTEGRATION.md) |
| Features overview | [`docs/FEATURES.md`](./docs/FEATURES.md) |
| FAQ / troubleshooting / dev | [`docs/FAQ.md`](./docs/FAQ.md), [`docs/HAVING_TROUBLE.md`](./docs/HAVING_TROUBLE.md) (end users, no terminal), [`docs/TROUBLESHOOTING.md`](./docs/TROUBLESHOOTING.md) (maintainers), [`docs/DEVELOPMENT.md`](./docs/DEVELOPMENT.md) |
| **On-call runbook** (incident decision tree, supervision, memory budget, glassbox map, bench noise bands, sibling-rebuild map) | [`docs/RUNBOOK.md`](./docs/RUNBOOK.md) |
| Retrieval pipeline steps + env-knob registry (GENERATED, freshness-gated) | [`docs/retrieval-pipeline.md`](./docs/retrieval-pipeline.md) |

### Notable in-tree invariants

These are structural commitments enforced by tests, not policy
toggles. Code reviewers should call out attempts to bend them.

- **KnowledgeView privacy** is layered three deep — recipe
  (`scope=local`, `mesh_sharing=false`), acquirer SQL (excludes
  `local_only` skills at ingest), splice (suppresses non-personal
  views when the active skill is `local_only`). See
  [`docs/knowledge-view.md`](./docs/knowledge-view.md).
- **Watched folders are read-only on source**; sensitive folders
  are excluded from ambient retrieval (Filter 3 in
  `Runtime::search_corpus_indexes`); multi-root corpora dedup by
  content hash. Invariants pinned in `local_corpus::watched::`
  tests.
- **Single-flight chat dispatch** — `ChatView` dispatches
  `SEND_INITIATED` before any bridge await (fixes the 60s
  blank-window bug); `ensureConversation` uses `CONVERSATION_BOUND`,
  not `HYDRATE`, to preserve the in-flight user bubble.

### Agent-coding battery + canonical tools + TDD

`sovereign/crates/sovereign-agent-bench/` — eleven-problem graded
battery measuring end-to-end coding agents (pi / opencode / codex /
aider, model-agnostic): six problems in Rust / Go / TypeScript plus
five Python variants (fixtures under
`sovereign/bench/agent-coding/problems/`). Judged `0..=3` against
anchor rubrics on three dimensions per problem (9/problem, 99 max). CLI:
`svrn agent-bench <run|list|show>`. Dispatch via
`AgentRunnerRegistry`.

`sovereign/crates/commonwealth-agent-tools/` — canonical tool
surface. Ten primitives (`inspect_workdir` polymorphic over
file/dir/find/grep, `write_file`, `patch_file`, `replace_function`,
`build`, `smoke`, `agent_done`, `agent_plan`,
`handoff_to_evaluator`, `handoff_to_implementer`); every runner
translates to/from this set. Plus a
role layer (Planner / Implementer / Evaluator) operating on the
same model weights via different prompts + tool subsets + forced
first tools.

`sovereign/crates/commonwealth-tdd/` — unified solver loop for any
TDD-shaped workflow. One function `run_trial(Trial) → TrialResult`
with `Polarity::{MaximizePassing, GenerateOneFailing}`
(`run_trial_observed` adds a per-round observer for live progress).
`tasks::solve` is the verbless goal entry: failing tests → fix;
none → pin-then-green via `bdd_cycle`; explicit verbs `fix` / `pin`
/ `split`. See [`docs/TDD_MACHINE.md`](./docs/TDD_MACHINE.md).

**SOLVE surface** (`docs/specs/SOLVE_UX.md`) — the daemon hosts the
solver as an async job API on `:9741`: `POST /v1/solve/jobs` (202 +
detected framework/test-command/model), `GET .../{id}` (state +
rounds + result), `GET .../{id}/events` (SSE round/done), `DELETE`
(cancel). Loopback-only; 1 job per workdir, 2 global; backend = the
daemon's own `/v1/chat/completions`. Job host:
`sovereign-cli-daemon/src/daemon_cmd/solve_http.rs`; MCP tools
`solve` / `solve_status` / `solve_cancel` in `solve_tools.rs`; CLI
`svrn solve <workdir> "goal" [--watch]`
(`sovereign-cli-llm/src/solve_cmd.rs`). The synchronous
`POST /v1/solve` + `tdd_solve` MCP in `sovereign-server` stay for
back-compat. User guide: `docs/SOLVER_FOR_PI_USERS.md`.

---

## 5. cmnwlth — the coordination daemon

A symmetric daemon. Every node runs the same binary; no master.
Members find each other via mDNS on the LAN or transitively over a
VPN (Tailscale/WireGuard) and converge on shared state via gossip.

In one sentence: translates "complete this chat with model X" into
a plan that spawns `llama-server` on one node and `rpc-server` on
others, holds the OpenAI-compatible HTTP endpoint open, and keeps
the plan healthy as nodes come and go.

### Discovery and membership

- **Join keys** — `cwth-XXXX-XXXX-XXXX`.
  `membership::generate_join_key` stores BLAKE3 hash, discards
  plaintext. `verify_join_key` compares BLAKE3 hashes. First node calls
  `init_mesh`; subsequent nodes call `accept_join`.
- **Node identity keys** — every node persists an Ed25519 keypair at
  `<data_dir>/node_key` (sibling of `node_id`;
  `commonwealth-transport/src/identity.rs`). The pubkey travels as
  `MemberRecord.node_pubkey` (serde-defaulted — pre-identity builds
  interop), is proven at join time (proof-of-possession signature in
  `JoinRequest`, bad proof → 401), self-stamped by gossip every round
  (the in-place upgrade path), and protected by an anti-downgrade
  rule in `Mesh::merge_from` (a relayed record without the key never
  strips a known one). The seed is byte-compatible with an iroh
  `SecretKey` — it IS the future dial-by-key transport identity.
- **mDNS** — `_commonwealth._tcp.local` advertising `node_id`,
  `mesh_id`, `name`.
- **Gossip** — 10s epidemic loop, 2–3 random peers per round.
  Three-phase digest/delta/response. Conflicts: timestamp LWW.
  Payloads: `MemberState`, `InferencePlan`, `KnowledgePlan`,
  `LedgerEntry`, `MeshConfig`.
- **Latency probing** — UDP RTT every 30s, magic bytes `CWLP`,
  EWMA α=0.3. `LatencyMatrix` shared via gossip.
- **Hardware detection** — `commonwealth-discovery/src/hardware.rs` tries
  `nvidia-smi`, then `rocm-smi`, then Metal.
- **TLS / mesh encryption** — A plaintext mesh (the default) serves
  the internal API (:9742) in the clear; the unused per-session-cert /
  `TrustStore` scaffolding (`discovery/tls.rs`) was removed (2026-06-15)
  rather than left as a security façade. TLS *is* used on the separate
  worker-pod path (`sovereign-mesh/worker_daemon.rs`,
  `axum_server::bind_rustls`).
  - **Encrypted mesh (opt-in, founder-set at creation).** A mesh created
    with `require_encryption` flips every node to the iroh dial-by-key
    transport (QUIC/TLS) in REQUIRE mode — no plaintext fallback
    (`RoutedTransport::with_required`, fail-closed) — binds its internal +
    client listeners loopback-only (the iroh acceptor is the sole network
    ingress), and admits joiners only over an encrypted, founder-key-dialed
    channel with a short-lived (24h) TTL invite. The policy lives on the
    gossiped `Mesh` struct (`require_encryption`, monotonic stricter-wins
    in `merge_from`), is inherited at join, and persists. Dial info
    (`relay_url` + `iroh_direct_addrs`) is signed per-node
    (`commonwealth-core::dial_sig`, monotonic `dial_info_version`) so a
    gossip-strip attacker past the join-key gate cannot force a peer
    unreachable or downgrade it. **NOT covered:** the multi-host
    tensor-split RPC between `llama-server`/`rpc-server` (raw TCP, out of
    the transport seam) — the sole residual plaintext on an encrypted
    mesh. Never claim blanket end-to-end encryption. The consolidated
    surface-by-surface posture (every listener, its default bind, auth,
    and the honest gap ledger) lives in `../docs/THREAT_MODEL.md`.
- **Mesh peering** — `peering.rs`; two `PeerTrustLevel`s:
  `ModelAndKnowledgeSharing`, `Full`.

### The PeerTransport seam (commonwealth-transport)

How this node reaches a peer is decided in exactly one place:
`commonwealth-transport`'s `PeerTransport` trait resolves
*(PeerContact, TrafficClass) → ordered base URLs*; call sites keep
their own reqwest clients/timeouts and append route paths. The live
instance hangs off commonwealth-api's `AppState`
(`peer_transport()` / `install_peer_transport`).

- **`IpTransport`** (production): today's tailnet/LAN overlay. Owns
  the Tailscale CGNAT/ULA address ranking
  (`commonwealth_core::peer_addr::rank` — no other production caller)
  and the last-working-address promotion
  that used to live as duplicate caches in gossip and knowledge
  fan-out. Port policy per class: Gossip/ControlPlane/
  KnowledgeSearch/ModelTransfer use the gossiped address verbatim;
  Inference/StatusProbe rewrite to the (assumed-uniform) client
  port. Golden URL-vector tests pin byte-identical output vs the
  pre-seam inline `format!` strings.
- **`IrohTransport`** (cargo feature `commonwealth-transport/iroh`,
  pinned `iroh 1.0` stable since 2026-06-18): dial-by-Ed25519-pubkey
  QUIC, bridged to HTTP via localhost byte-tunnels (client TCP bridge
  + `IrohAcceptor` → existing axum listener). `IrohAcceptor` has two
  forms: `spawn` (all streams → one local listener, Track M) and
  `spawn_routed` (W1 — dispatch by negotiated ALPN to per-class local
  listeners). Spike proof:
  `sovereign-mesh/tests/iroh_transport_e2e.rs` (run with
  `--features iroh-experimental`) drives a real gossip round dialed
  by pubkey. `IrohTransport` resolves its dial target from the
  gossiped `PeerContact` (relay + direct addrs, W2) and picks the ALPN
  by `TrafficClass`; whether it carries a class is the W3 config flip.
- **`RoutedTransport`** (`commonwealth-transport/src/routed.rs`, W3):
  routes each `TrafficClass` to a chosen transport, concatenating its
  candidates ahead of a default (`IpTransport`) — callers try in order,
  so a failed/absent iroh dial degrades to the tailnet path on the same
  request, automatically. `note_success` routes feedback to the
  producing transport by label prefix. Empty `per_class` == its default.
- **Track W1 (server half) + W2 (dial info in trust ring) + W3
  mechanism are implemented** (2026-06-18): when `[iroh] enabled`,
  `EmbeddedDaemon::start_daemon` binds one iroh endpoint from the
  daemon's gossiped `node_key` and `spawn_routed`s it across both
  ALPNs — `cwth/http/0` → internal router, `cwth/client/0` → client
  router — so a peer/phone reaches this daemon by key with no VPN
  (`sovereign-mesh/src/iroh_access.rs`, `MeshIrohAccess`; additive,
  fail-soft, held in `DaemonState::Running`). W2: `MemberRecord` carries
  `relay_url` + `iroh_direct_addrs` (serde-defaulted, MUTABLE
  reachability — normal LWW, unlike `node_pubkey`'s anti-downgrade);
  the daemon self-stamps its live dial info each gossip round via a
  pull-provider on `AppState`; `IrohTransport` dials peers purely from
  the gossiped contact (**membership = dialability**).
- **The no-VPN mesh (2026-07, merged via the `Saas` PR #13; unit +
  e2e + soak-axis verified).** When iroh is enabled, `RoutedTransport`
  routes **every** `TrafficClass` iroh-first with automatic per-dial
  IP fallback — `[iroh.transport] <class> = "ip"` is now an opt-OUT,
  not an opt-in flip. Enablement is `[iroh] enabled: Option<bool>`:
  absent = AUTO (on iff this node is in a mesh, keyed off the
  `client-exposed` marker — a meshless daemon never contacts relays),
  `Some(false)` = kill-switch (also `SOVEREIGN_IROH=off`). Plaintext
  invites carry a `dial=` connect code (distinct from the encrypted
  `iroh=`); `join::perform_join` dials the founder by key first and
  falls back to `?relay=`/mDNS (W2c). A `RelayConfig` (`[iroh]
  relay_urls` + `discovery`) drives `build_relayed_endpoint`:
  self-hosted relays (W4), and `discovery = "none"` builds from
  `presets::Minimal` to sever ALL n0 services (H1 — `relay_urls`
  alone keeps n0's DNS lookup, so it is not a no-third-party
  posture). `proxy_from_env` is always on, so the mesh survives
  UDP-blocked corporate networks over relay-TCP:443 through a
  (Basic-auth) HTTP proxy. Encrypted meshes
  (`require_encryption`) stay the fail-closed variant (all classes
  REQUIRE iroh, loopback-only listeners). `IpTransport` remains the
  permanent fallback; every piece is config-reversible.
- **Track M (mobile) is implemented**: `sovereign-server`'s
  `[iroh] enabled` block accepts dial-by-key clients on ALPN
  `cwth/client/0` (`src/iroh_access.rs`; pairing string at
  `GET /status` → `iroh.dial`), and `sovereign-mobile`'s
  `endpoint_kind='iroh'` host rows tunnel HTTP+WS through a
  localhost bridge (`src-tauri/src/iroh_bridge.rs`) — no VPN on the
  phone. This pulls the iroh feature into sovereign-server's default
  build (runtime-gated off); see
  [`docs/specs/TRANSPORT_MIGRATION.md`](./docs/specs/TRANSPORT_MIGRATION.md)
  for phase status and device-side exit criteria.
- **Out of seam, by design**: the join handshake (pre-membership
  bootstrap), worker-pod `PinnedTransport` (separate trust model),
  loopback self-probes, and the raw-TCP `llama-server`/`rpc-server`
  tensor traffic. The last is the decided W6 posture: multi-host
  inference needs its GPU anchors on a shared IP network (LAN/VPC) —
  which every supported topology already has — rather than a VPN or a
  per-worker iroh sidecar (specced as Option B, gated on a tok/s A/B).
- **Migration order** (when a second transport goes live) is encoded
  by `TrafficClass`, not config: a small `RoutedTransport` mapping
  classes → transports slots into the same `Arc<dyn PeerTransport>`
  — gossip/membership first, blob/model transfer next, inference
  streaming last, raw RPC tensor traffic remaining on IP. The full
  phased plan (mobile first, then per-class mesh flips, relay
  self-hosting, Tailscale-optional end state) is
  [`docs/specs/TRANSPORT_MIGRATION.md`](./docs/specs/TRANSPORT_MIGRATION.md).

### Scheduling + orchestration

**The live decision topology** (rationalized 2026-06-10 — a dead
second-generation scheduler that previously filled this section was
deleted; see `docs/specs/OICP_RATIONALIZATION.md` for the audit):

| Decision | Where | Mechanism |
|---|---|---|
| Joiner decides a turn is offload-*eligible* | `sovereign-mesh/oicp_select.rs::offload_eligible` | SLOT_POLICY §5: `privacy == MeshAllowed && latency_class != Fast`. One predicate, shared by `select_peers_ranked` and `shared_primary_id`; replaced the old privacy-gate + `preferred_speed != Slow` pair (the Speed shadow no longer gates routing) |
| Joiner picks peer-vs-local for an eligible turn | `sovereign-mesh/peer_inference.rs::select_peers_ranked` | OICP claim score × operational adjustments (observations, load, locality, cold-start, throughput, availability); forced-choice sentinels exclude peers not advertising `x:forced_choice` |
| Joiner resolves a *named* target | `sovereign-mesh/peer_inference.rs::locate_named_model` | Name resolution + min-in-flight tiebreak, **not** the scorer. **Hard** (caller-supplied `model_id`) is a constraint: unknown ⇒ error, never substitution. **Soft** (configured `shared_model_id`) is a preference: unknown ⇒ falls THROUGH to `select_peers_ranked` with local as the last rung, recorded on `DecisionPath::NamedFallthrough` (SCHEDULER_QUALITY F8 / §4.3, 2026-07-27) |
| Hub picks a local model for a peer request | `commonwealth-api/routes_inference.rs::route_with_oicp` | OICP claim score over synthesized claims |
| Serving peer picks Fast-vs-Slow slot | `sovereign-mesh/oicp_select.rs::pick_slot_for_oicp` | canonical `slot_policy::latency_to_speed` + hint veto; `pick_slot` backstops `x:forced_choice` sentinels onto Primary |
| Synthesis tier (Fast vs Primary) | `sovereign-core/runtime/evidence.rs::resolve_synthesis_route` | intent + atom-enum + evidence-shape heuristic |
| Distributed placement (model > one node) | `sovereign-inference/embedded/rpc_distribution.rs` | LocalOnly default; StreamSplit ≤500MB; warmed owned-overrides as last resort |
| Collaborative ingest partitioning | `commonwealth-inference/scheduler/knowledge_assignment.rs` | `plan_collaborative_ingestion*`: embed-model-compatible peers, storage-proportional contiguous blocks, zero-storage peers skipped |

**Slot policy is normative** in [`docs/SLOT_POLICY.md`](./docs/SLOT_POLICY.md)
(OICP-first rationalization, 2026-07-08): call sites declare a
`slot_policy::Workload` requirement bundle rather than free-handing
`Speed::` literals; the scheduler resolves those against every slot's
advertised claims cluster-wide, and fast-vs-primary is an emergent
scoring outcome (the local node is the degenerate one-node mesh).
`Speed::Medium` is retired as a construction target (kept only for
serde/metadata); the one canonical `latency↔Speed` map lives in
`sovereign-contracts/slot_policy.rs`.

The composed OICP scoring product lives ONCE in `oicp-types`
(`score_with_adjustments` + `ScoreBreakdown`, Phase B of the
rationalization) and is consumed by sovereign-mesh and
sovereign-inference; leader election lives in
`commonwealth_core::partition::elect_leader`.

**Scheduler quality — measurement, not just plumbing.** Retrieval,
grounding and synthesis each have a bench and a tight iteration loop;
this layer has unit tests on individual factors and e2e suites that
verify *plumbing*, and until 2026-07 nothing measured whether a
routing decision was **good**. The diagnosis, the six findings behind
it and the build order are in
[`docs/specs/SCHEDULER_QUALITY.md`](./docs/specs/SCHEDULER_QUALITY.md);
the root cause is that `score_with_adjustments` returns a product of
six dimensionless multipliers — it *ranks*, it does not *predict* — so
no scoreboard was definable.

**Phase 0 (instrumentation) is landed**, changing no routing decision:

| module | role |
|---|---|
| `sovereign-mesh/decision_log.rs` | One `RoutingDecision` per decision point (whole candidate set, each `ScoreBreakdown`, each input stamped with its **provenance and age**, peers excluded before scoring and why, the verdict) joined by `decision_id` to one `RoutingOutcome` per completion (served-by / TTFT / total / tokens / shed / failovers). `DecisionSink` is the seam — production, capture-for-tests, null. |
| `sovereign-mesh/decision_trace.rs` | Replay: `SchedulerTrace::from_jsonl` groups records into `Episode`s by `decision_id` (never adjacency — a live log interleaves requests) and reports a `join_rate` to gate on. |
| `sovereign-mesh/peer_inference.rs` | Emission at `select_peers_ranked` (including gated exits) and join-closing in both stream cascades and `complete`. `observation_snapshot()` exports per-peer observations + gossiped benchmarks + `PeerHealth` — folded into the record stream every 60s so a capture is self-contained. |

Capture with `SOVEREIGN_DECISION_LOG=<path>` on the daemon; records
also reach `tracing` under the **`mesh.decision`** target (listed in
`DAEMON_TRACING_FILTER`, without which a custom target is dark).

**Phase 1 S0 (the Tier-1 simulator) is landed** (2026-07-26), and it
changed the diagnosis:

| module | role |
|---|---|
| `sovereign-mesh/scheduler_core.rs` | The routing decision as a **pure total function** — `rank(DecisionBuilder, RankInputs) -> RankResult` over a snapshot of what a decider believes, with `now_unix` passed rather than read. `select_peers_ranked` is now gather-then-decide: async I/O above the line, this below it. Also holds the observation feedback (`observe_dispatch` / `observe_success` / `observe_failure`) the provider's `record_*` methods delegate to, so sim and production age their beliefs by one implementation. |
| `sovereign-mesh/mesh_sim/` (feature `mesh-sim`) | Seeded discrete-event mesh: virtual clock, gossip propagation, manifest-cache ageing, queueing, **model-load time** (`model_load_sec_per_gb`: a cold node advertises `loaded: false` + an estimate and pays it once, attributed to TTFT so the throughput EWMA is not poisoned), nineteen arms (as-implemented / **blind-local-load** / **blind-peer-ramp** / **blind-observations (§4.4)** / fresh-signals / two-choices / both / warm-start / fresh+warm-start / outbound-only-load / **predicted-time (§4.1)** / predicted-time+outbound-only / **tier-floor** / **predicted-time+tier-floor (§4.1.1)** / **predicted-time+tier-floor+two-choices (§4.1.2)** / **predicted-time+tier-floor+within-noise (§4.1.3)** / **response-backpressure (§4.2.1)** / predicted-time+tier-floor+backpressure / a perfect-information oracle). Arm 0 *is* `rank` — not a transcription of it, but note it models the beliefs the dispatch path was *designed* to produce; the three `blind-*` arms model the ones it actually produced before F9's fix, and `blind-observations` is the as-shipped baseline (§4.4). No extra dependencies; **four** separate RNG streams — world, policy, advertised-rate error, and advertised-size error — so switching arms cannot perturb the world the arms are compared in, and both fidelity knobs default to inert so every number recorded before they existed still reproduces. |
| `sovereign-mesh/predicted_time.rs` | The §4.1 candidate objective, and the only ranking in the tree with **no tunable constant**: `predict()` returns `queue + prefill + decode + rtt` as named addends or an `Unpredictable` reason (never a defaulted rate — a guessed rate is a fabricated fact with a unit attached), and `faster_than_local` filters on it. `LocalOption` keeps *unpredictable* local (⇒ no hop) distinct from *infeasible* local (⇒ any feasible peer wins); collapsing those points them in opposite directions. Reads only what a decider can see, so `PredictInputs::from_candidate` scores it against a production capture. |
| `sovereign-mesh/mesh_sim/scoreboard.rs` | `RecordMetrics` and `TierMetrics` are computable from a **production capture** too (the S1 precondition) — `TierMetrics` is §5's capability column, counting downgrades and declined upgrades from decision records alone; `TruthMetrics` needs simulator ground truth and so may never define a calibration gate. |

Run it: `cargo test -p sovereign-mesh --features mesh-sim,treesitter
--test mesh_sim_scoreboard -- --nocapture` (~0.3s; `sovereign-lint.sh`
keeps it compiling).

**Phase 1 S1's instrument is landed** (2026-07-26) — the hardware
capture it points at is not taken:

| module | role |
|---|---|
| `sovereign-mesh/decision_replay.rs` | Re-runs the **live** scorer and the **live** ranking policy over a captured record and reports whether the record reproduces its own scores and verdict. Split in two on purpose: *scorer agreement* (recorded `CandidateInputs` + `claim_score` + locality → `score_with_adjustments` → does `final_score` come back?) and *policy agreement* (recorded scores → `winners_over_local` → does the `Verdict` come back?). The two run off independent inputs so one bug cannot cascade into the other. Both ratios return `0.0` on an empty denominator, never a vacuous `1.0`. |
| `sovereign-mesh/scheduler_core.rs` | Gained `winners_over_local` / `beats_local` / `local_sentinel` — the ranking half extracted so replay re-runs the policy rather than a copy of it. Also `RankObjective` on `RankInputs` (`Product` \| `PredictedTime`): the objective is a *parameter* rather than a branch at the call site, so both objectives share one scoring body, one record shape and one `finish_at` — which is what keeps a decision record describing what the decider actually did instead of the product's opinion of a choice it did not make. Production passes `Product`. |
| `tests/scheduler_replay_agreement.rs` | The fixture with a known answer: sim → `TracingDecisionSink::to_path` → JSONL → `SchedulerTrace::from_jsonl_path` → replay. **1.000 / 1.000, bit-exact**, five scenarios × six arms. |

The gap S1 was expected to surface — `claim_affinity` is an argument
the scorer takes and the record does not carry — turned out not to
need a schema field: `observation_mult = effective_affinity(a,obs)/a`
is independent of `a` over `(0, 1]`, and `a` is clamped to `[0, 1]` at
the type level by `CapabilityClaim::effective_affinity`. Settling that
in the simulator is the reason the replay was built before the
capture.

Three **diagnostic** arms landed with it, each pricing a question
before it costs hardware time, and each asserting its own wiring first
(a null result is only informative if the knob is proven connected).
`WarmStart` prices F7: removing the cold-start floor is **+235% mean
latency**, so the floor is the mesh's only brake on offloading.
`FreshWarmStart` then asks whether that damage is F1's — and says
**no**, the penalty is *larger* (+264%) with a perfect load signal, so
the extra offloads lose on their own merits and the floor is
compensating for an **over-eager objective**. That is a direct
argument for §4.1's structural change: a product of dimensionless
multipliers cannot decline a hop that costs more than it buys, and
ranking on predicted time-to-answer can. `OutboundOnlyLoad` says that
if the gossiped in-flight counter misses inbound peer work it costs
+126% to +584%, which earns the two-daemon audit F2's caveat now calls
for.

Findings, in `SCHEDULER_QUALITY.md` §3.1: **F3 reproduced exactly**;
**F1 reproduced but costs the tail, not the median** (the reverse of
the hand-model's reading); **F5's mechanism reproduced but its
two-choices remedy is inert** wherever the fleet has a unique
capability winner, because the eligible set is then a singleton; and a
new **F7 — the cold-start ramp is self-locking**, contradicting
`cold_start_weight`'s own doc comment. **No Phase-2 behavioural change
has landed in production** — the ordering is deliberate: the sim is the
baseline machine, so fixes land as sim arms first.

**§4.1 measured (`Arm::PredictedTime` + `sovereign-mesh/predicted_time.rs`,
2026-07-26).** The candidate objective — rank on
`queue + prefill + decode + rtt` instead of on a product of
dimensionless multipliers — now exists as an arm, introduces **no
tunable constant**, and is computable from a decision record, so it can
be scored against a production capture with no new instrumentation.
It decomposes the oracle gap that arm 0 and `Oracle` only bracketed:
**the wrong objective costs +126%/+200%/+250%, imperfect information
costs +4.7%/+1.8%/−0.0%** (household / twin-hubs / heterogeneous), which
demotes F1 to the sustained-contention case (`isolation`, +43.8%). The
win survives a ±2× mis-rated fleet (`SimConfig::advertised_rate_error`,
which exists because the sim otherwise grades the predictor against its
own rate card). **It cannot land yet, and not for a latency reason:**
ranking on time alone routes knowledge turns to 4B laptops — 37 of 38
household offloads, and never a hub in `twin-hubs` — so §4.1's tier
floor is a prerequisite, and no §5 metric can see what its absence
costs. Replay also surfaced a missing field: a `RoutingDecision` does
not record *which objective* produced its verdict, so a predicted-time
capture reports scorer agreement 1.000 and policy agreement 0.009.

**The tier floor, and what it did to that claim (`sovereign-mesh/tier.rs`,
`Arm::TierFloor` + `Arm::PredictedTimeTierFloor`, 2026-07-26 — full
result in `SCHEDULER_QUALITY.md` §4.1.1).** Capability is now a
**filter, not a term**: candidates are partitioned into bands derived at
runtime from the sizes on the manifests a decider currently holds — a
*relative* edge (`BAND_RATIO`, measured against the band's max),
recomputed per decision, never an absolute GB threshold or a table of
model names — and a `Normal`/`Extended` request must be served from
band 0. The floor is read off `effective_latency_class()`, the same map
`latency_to_speed` already uses locally, so this is the policy the local
slot picker has always enforced, finally applied to peers. `TierMetrics`
adds §5's missing column and splits two hazards that were being counted
as one: **downgrade** (served below the origin's own local model — a
real regression, 31% under predicted-time) versus **declined upgrade** (a
stronger node was feasible, 69%). Both are computed from decision
records alone, so the identical function scores a production capture.
Three results change the plan: **(1)** §4.1's headline is not
quality-constant — on `twin-hubs`, the one fleet whose top band is not
saturated, arm0+floor is 31.0s against predicted+floor's 32.6s, so at
constant quality the objective is *~5% worse* than the product, not 200%
better; **(2)** the floor is *free* where the top band has capacity
(twin-hubs −2% versus arm 0 with every quality loss eliminated) and
catastrophic where it is not (household 25.7s → 559.5s) — but
`queue_wait_ms` by dispatch quartile shows a **flat** service time
against a queue climbing 241s → 1020s, so that is a capacity fact about
a one-hub fleet, not a scheduling result, and `heterogeneous-fleet`'s
queue is already unbounded under arm 0 with no floor at all;
**(3)** predicted-time *herds harder* than the product once the floor
makes candidates homogeneous (40/28/10 across three identical hubs
versus 31/27/18), so §4.2 step 2 is a prerequisite rather than a
follow-on. `SimConfig::advertised_size_error` prices the floor's own
self-reported input the way `advertised_rate_error` prices the rate
card.

**What the objective is actually worth, on a second unsaturated fleet
(`scenario::mixed_hubs`, `Arm::PredictedTimeTierFloorTwoChoices`,
2026-07-27 — full result in `SCHEDULER_QUALITY.md` §4.1.2).** Result (1)
above was n=1 in two ways at once: one fleet, one seed. At five seeds
across two fleets the answer is **conditional on whether the top band's
members differ in speed**. `mixed-hubs` is the second unsaturated fleet
and the deliberate *opposite bracket* to `twin-hubs` — the same 35B (so
the same band) on 34/25/11 tok/s machines, where `twin-hubs` band 0 is
three identical hubs. Predicted-time is **+3% (1/5 seeds)** on
`twin-hubs` and **−8% (5/5 seeds)** on `mixed-hubs`. The mechanism is F3
and it is not the obvious one: the product already sends **zero** turns
to the 11 tok/s hub (`throughput_factor` 0.55 is decisive), and its
whole loss is splitting ~50/50 between the 34 and 25 tok/s hubs, which
the clamp at 20 tok/s renders identically 1.0 — deleting the slow hub
leaves predicted-time ahead by 3%, so the win survives deleting the gap
the scorer *can* see. It is not the harness flattering the objective
either: under `advertised_rate_error` the win *widens* (−8/−7/−11/−13%
at ±0/25/50/100%), and the product's one error-correcting path (observed
decode EWMA past five samples) is shown to carry only ~5% of scorings.
Result (3)'s remedy is measured rather than inferred: a **blunt**
two-choices sampler takes `twin-hubs` from +3% to −4% and `mixed-hubs`
from −8% to +3%, so §4.2 step 2's *"among candidates whose predictions
are within noise"* is the load-bearing clause, not a refinement — and
what makes that clause expressible is that predicted times have
**units**, where a dimensionless product has no scale on which two
scores can be called close. Saturation is now gated on
`backlog_depth` (final-quartile queue wait over service time: household
38 turns, heterogeneous 6.6, twin/mixed both under 1.0); the earlier
Q1→Q4 3× ratio is kept only as a screen, because it fires on any fleet
loaded enough to build a queue at all.

**Fresh backpressure measured before it was built — and deferred
(`Arm::ResponseBackpressure`, 2026-07-27 — full result in
`SCHEDULER_QUALITY.md` §4.2.1).** §4.2 step 1 proposed collecting
`fresh-signals`' −9..−22% p95 by piggybacking the serving node's load
onto responses it already sends. The arm is that mechanism with its
*real* reach — fresh only for a peer this decider has actually served a
request through — and it does not pay: **+1.6/−2.6/+0.1% mean** across
household-evening-12 / twin-hubs / mixed-hubs at **4–7% dispatch
coverage**, against fresh-signals' −9 to −11%. The mechanism is not
broken, it is *unreached*: on `isolation` (a background actor
dispatching every ~8s) coverage rises to **46%** and the median true
signal age drops 15.0 → 10.4s, so coverage is a property of traffic
density, not of wiring. The two densities form a scissor — where it
fires the fleet is capacity-bound (fresh-signals itself buys −1.8%
there), and where information binds it does not fire. The structural
reason generalises: a response can only carry news about a peer you
**already chose**, and F1's cost lives in the peers you did not. §4.2's
prediction that freshness matters more to the predicted-time objective
(bounded `load_penalty` vs. a first-order queue term) is real in the
arithmetic and invisible at this coverage: −2.6% vs −2.5% on twin-hubs.
Deferred rather than retired because the **503 body** — the case where
the reading is about a peer you were about to keep hammering — is
untestable in a sim with no admission gate, exactly as F4 is; the
piggyback should ride §4.2 step 3's shed path instead.

**F10 — the scheduler has no speed signal in production, and it changes
how every paragraph above should be read (2026-07-27 — full result in
`SCHEDULER_QUALITY.md` §4.5).** Everything above is Tier-1: measured on
a simulator where each node advertises a `BenchmarkResult`. **No node on
this mesh ever has.** `run_baseline_benchmark`
(`sovereign-inference/src/benchmark.rs`) had zero callers,
`set_local_benchmark` (`peer_inference.rs`) had zero callers, and
`build_local_capabilities` hardcodes `benchmark: None` into every gossip
tick — under comments that used to describe the startup probe and a
`with_benchmark` setter as though both existed. Neither did; both
comments are now corrected in place.

**As of 2026-07-28 both dead producers are deleted**, along with
`MeshInferenceProvider`'s `local_benchmark` field, so the local
`LocalCandidateView.benchmark` is now a literal `None` with no state
behind it. Production is blind by *construction* rather than by
accident, which is what makes the `blind-shipped` arm a measurement of
the shipped system rather than of a state it merely happens to be in.
Leaving the probe in place was the standing invitation the paragraph
below argues against.

So `throughput_factor` has two sources and production supplies neither
(the observed decode EWMA is gated behind a `samples >= 5` the ranked
path never reaches for a peer), leaving it at **neutral 1.0 for every
peer on every fleet**. Read the `mixed-hubs` sentence above with that in
mind: "the product already sends zero turns to the 11 tok/s hub,
`throughput_factor` 0.55 is decisive" is true of the *simulated* mesh
and false of this one, where that hub scores 1.0 like everything else.
F3 is not a weak term; it is a constant.

Two arms price it (`blind-rate-card`, and `blind-shipped` = the mesh as
it runs tonight, now the as-shipped denominator in place of
`blind-observations`). The rate card is worth **0% on five of six
fleets** — including `heterogeneous-fleet` — and **−32% mean on
`mixed-hubs` alone**, because the clamp at a 20 tok/s reference means a
card only carries information about a node *slower* than reference, and
`mixed-hubs` is the suite's only fleet containing one.

**The obvious repair is a measured regression, so it is filed
DO-NOT-BUILD.** Adding a call site to the (now deleted) probe wires the
`Speed::Fast` slot: a ~2.5 GB model's rate stamped in as the baseline,
which `throughput_factor` then extrapolates up to a 21 GB candidate on a
*linear* size law. Decode is bandwidth-bound and the law is false, and
the term's clamp is one-sided, so the error can only push large models
down. New knobs `SimConfig::probe_baseline_size_gb` /
`probe_sublinearity` measure it as `rate ∝ size^-β` (β=1 is the code's
own assumption and reproduces the un-probed rows exactly): at β=0.7 the
"win" grows to −56% while declined capability upgrades double 31→67, and
at β=0.5 real downgrades appear. An honest card costs no quality — but
only if it describes the model being *scored*.

`svrn mesh bench` (below) is the per-model measurement that condition
asks for, and it **deliberately does not write here.** Its number is
aimed at a human deciding whether to add a machine, not at the ranked
dispatch — and `throughput_factor` would extrapolate away from it
through the same one-sided clamp the moment it arrived. Same number,
different consumer. Pointing it at `NodeCapabilities.benchmark` ships
this section's regression with no other code change; a future reader who
"completes the wiring" while citing §4.5 correctly will have aimed it at
the wrong target.

This also settles why §4.1 cannot ship: `PredictInputs::from_candidate`
reads the advertised benchmark and nothing else, so unhardcoding
`RankObjective::Product` today would yield `Unpredictable::NoThroughput`
on every candidate of every request. The hardcoded switch is not the
blocker; the missing rate card is.

**Shared-model fleet churn/failover hardening (Phase 3).** A fleet sharing one
distributed primary stratifies into anchors (hold the RPC layer-split) + a
consumer ring. Anchors advertise `NodeCapabilities.anchor` (`AnchorProfile{
can_anchor, vram_gb, model_resident }`, populated env-derived in
`build_local_capabilities`); `discover_rpc_workers` filters candidates to
`can_anchor` so a casual peer never joins the split, and anchors get the
stricter `worker_eligibility::EligibilityConfig::anchor` profile (settle 300s,
quarantine on first flap). The RPC reload loop (`daemon_cmd::bootstrap`) does
**shrink-fast-prune** — an anchor dropping out of the loaded set reloads
immediately (prune before `GGML_ABORT`) while pure grows keep the 20s debounce.
**Host failover:** every anchor runs the discovery loop but only the elected
host distributes — `partition::should_host(self, host_node_id_pin, eligible_anchors)`
(pin wins while eligible, else `elect_leader`), re-evaluated each tick over
gossiped membership, published to `GET /v1/mesh/status` (`shared_model_host`) so
the mesh soak asserts the `shared_model_single_host` no-split-brain invariant.
Split-brain during convergence is bounded by the eligibility settle + the
quorum/pooled-memory gate (`InsufficientCluster` → "forming") + consumer
local-fallback. NOTE: the demoted-host model-teardown + full failover timing are
multi-box-only to validate (run `scripts/mesh-soak.sh`).

**Discovery never probes a worker over the link that worker's own tensors are
saturating (`daemon::reaffirm_plan`).** Gossip-Online membership — not a probe —
is the liveness signal for a worker discovery has already resolved once, because
the probe rides the congested path while gossip rides a separate one with a
looser budget. So a known **direct-ip** worker is re-affirmed from cache, and a
known **iroh-bridge** worker is re-minted from the transport's local bridge cache
(loopback only); only an unknown or probe-host worker pays the full `/status`
probe. Both known-worker cases trade the same way: a dead rpc-server behind live
gossip surfaces when ggml's RPC connection fails → supervised reload, not at
discovery. The bridge case is load-bearing because a non-direct endpoint gets no
stickiness (`sticky_endpoint` holds only direct-ip), so one starved probe used to
read as "worker absent" → flap → quarantine, compounding to 300s against a peer
that was serving throughout. Underneath, `HttpBridge::retarget` keeps a bridge's
loopback port stable across a peer's gossiped dial-info change (retarget in
place, don't rebuild), because that port IS the worker's endpoint string in
ggml's device list — minting a new one made an unmoved peer read downstream as a
stream of different workers.

**Byte-mass-aware split (`plan_shards_weighted`).** The placement policy apportions
each device a CONTIGUOUS block range whose *bytes* — not block *count* — are
proportional to its VRAM. This is the split the live load runs (`rpc_distribution`
overlays the model's real per-block byte mass from `rpc_warm_cache::tensor_sizes` on a
cache-miss, falling back to the count split only if the header read fails) AND the one
`mesh plan` previews — one function, so preview and reality can't diverge. It exists
because the big open-weight models are MoE, and MoE mass is deeply non-uniform: routed
experts (`blk.N.ffn_*_exps`, `is_routed_expert_tensor`) are **~88–93 % of the bytes**
but COLD (only the router's top-k run per token), and a hybrid SSM+MoE stack alternates
a ~20 MB attention/SSM block with a ~1.3 GB MoE block — a measured **62× per-block
spread**. Count-proportional apportionment (the old `plan_shards`, now the
`block_bytes == []` special-case) hands a small node a heavy contiguous run and OOMs it;
byte-proportional keeps each node ∝ its VRAM (e.g. 24 GB + 16 GB nodes → 18.6 GB + 12.3
GB of a 62×-spread hybrid). The output head is folded onto the host's budget. Ranges
stay contiguous, so single-stream decode keeps its **`D-1` hops per token** and a
layer's experts are never scattered across nodes (cross-node expert-parallelism would
add per-layer hops — wrong for single-stream; cold-expert→CPU offload buys nothing on
unified-memory APUs). See [[project_moe_byte_aware_split]].

**Pre-flight planning — `svrn mesh plan`** (`sovereign-cli-llm::mesh_cmd::cmd_plan`).
An offline dry-run of that split — a GGUF header-table parse, no model load and no GPU,
instant even on a 400 GB split — so you can see whether a model fits a mesh *before*
loading it. It shows the *bytes* each device holds and whether each one *individually*
fits. It also reports the MoE hot/cold
mass breakdown, whether per-block mass is uniform or skewed, and a **node/hop advisor**
— the minimum nodes that hold the model (fewest of the largest devices whose pooled
VRAM covers `model × headroom`) and the resulting hops, flagging when the mesh is spread
across more nodes than the mass needs. It frames this as a tradeoff, not a win button:
fewer nodes cut per-token hop *latency*, but net tok/s depends on the host — on a
memory-bandwidth-bound host (a unified-memory APU) offloading layers frees host
weight-read bandwidth and can raise *throughput* despite the extra hop (the measured
122B ran ~20% faster distributed 36/12 at 17.3–17.9 tok/s than solo at 14.8). So the
advisor reports the hop cost without claiming fewer nodes is always faster. `--from-mesh` plans across the
running mesh — each member advertises `vram_gb` + `can_anchor` on `GET /v1/mesh/status`
(`MemberDto`), the VRAM sourced from `rpc_distribution::local_gpu_total_vram_gb` (the
ggml device total, which unlike sysfs sees the full unified-memory pool on AMD APUs —
~124 GB on Strix Halo vs sysfs's ~0.5 GB dedicated carveout); `--devices 64,32,32` plans
a hypothetical mesh. The headroom factor is operator-set — `[shared_model] headroom` →
(bootstrap bridge) `SOVEREIGN_RPC_HEADROOM` → `rpc_headroom_factor()`, default 1.2,
replacing the hardcoded ×1.2 — and `mesh plan` defaults its `--headroom` to that same
resolution order, so the preview's headroom is the one the load executes with. Exit
codes: `0` fits, `1` won't fit, `2` bad args.
See [`docs/RUN_A_BIGGER_MODEL.md`](../docs/RUN_A_BIGGER_MODEL.md).

**Per-device fit — one decider, both sides (2026-07-28).** Until this date the
live host gated only on *aggregate* pooled memory (`pooled >= model_bytes ×
headroom`), so a cluster that cleared the aggregate gate could still hand one
worker more than it had; `mesh plan` caught that in its own private fold, which
meant the preview and the load could disagree about the thing the preview exists
to predict. Both now call **`rpc_warm_cache::shard_fits(plan, capacities, mass,
headroom) -> Option<Vec<ShardFit>>`**, beside the planner that produced the
split, over a `ModelMass` from `model_mass_from_sizes` (the same GGUF
header-table decomposition, replacing two divergent folds).

Three shapes are load-bearing. It returns **one row per shard, fitting rows
included** — a `Result<(), Overflow>` would force `mesh plan` to keep its own
traversal to print `ok +12.4 GB`, and a second traversal is the drift being
removed. `None` means **"cannot judge"** and is *not* a pass: an unread tensor
table would otherwise clear every device on the strength of zeros. And
capacities arrive in **plan order** (workers first, host last) while rows display
in the operator's `--devices` order — two permutations that look
interchangeable, pinned by a test.

`DistributionPlan` accordingly carries `device_vram_bytes: Vec<u64>` in place of
the summed `pooled_vram_bytes` (the sum is still what the quorum gate checks),
plus a `mass` computed **before** the plan-cache branch so a cached plan is
judged against the same numbers a fresh one is. A refusal is
`LoadPlacement::WorkerUnfit` / `PlannedDistribution::WorkerOverflow` /
`DistributedWarmOutcome::WorkerUnfit` — a **new** variant rather than a reuse of
`InsufficientCluster`, because pooling more memory does not fix an overflow and
saying "the cluster is forming" sends the operator looking for a peer that is
already there. `resolve_placement_inner` must **not** route it to `gate_local`:
falling back to a local load of an 80–90 GB model by a path that looks like
resilience is the 2026-07-27 session-kill. The compute-child path **parks**
(`bootstrap::park`, `retry_at = None`) rather than retrying, because an overflow
is not time-fixable — the existing worker-set-change re-plan is free and is the
only event that could change the answer. The refusal says **lower** the
headroom, not raise it (`need = held × headroom`), and names
`SOVEREIGN_SKIP_PER_DEVICE_FIT=1` for the one real false-positive: on a reload a
worker still holding its previous shard under-reports free memory.

**Measuring what you are running — `svrn mesh bench`**
(`sovereign-cli-llm::mesh_bench`). The producer for the `speed` block `mesh plan`
reports. **It measures the configuration that is loaded and never loads the one
it wants to measure** — there is no slot argument, so there is no slot to get
wrong, which is the mechanism satisfying `SCHEDULER_QUALITY.md` §4.5's "probe the
model being scored". An optional `<model.gguf>` is an *assertion*: fingerprinted
header-only against the resident primary, mismatch → exit 3 naming the config
line.

It fires real streaming completions at `POST /v1/chat/completions` and timestamps
SSE frames as they arrive, so the number includes the actual RPC split and iroh
path; `decode_tok_s = (content_frames − 1) / (t_last − t_first)`, steady state,
TTFT reported separately rather than smeared in. `prefill_tok_s` comes only from
the server's `usage.prompt_tokens` and renders `n/a` otherwise — never
`len()/4`. The probe (prompt, token budget, timing formula, guard set) is fixed
by `mesh_measurements::PROBE_VERSION`; there is deliberately no `--max-tokens`,
because a knob whose adjustment invalidates comparison against every prior record
while looking like harmless tuning is a trap.

**Nine validity guards**, six ported from
`scripts/measure-distributed-decode.sh` (each earned by an observed false result)
plus three new. Ported: which slot served it (below); per-frame timing; placement
re-read after the run; peer liveness before **and** after; a canary first; host
survival (from `/status` uptime going backwards, which unlike `pgrep` cannot
match a wrapper script or a deleted inode). New: `content_frames >= 32`,
inter-trial spread ≤25%, `finish_reason ∈ {length, stop}`.

**The served-slot guard, and why the obvious version of it does nothing.** The
shell script asserted that the SSE `model` field names the primary. On this
server that field is a **verbatim echo of the string the client requested** —
every frame says `commonwealth/primary` because that is what was asked for,
whatever actually answered. Measured 2026-07-28 on the first live run: with the
122B's compute child in `lifecycle: starting`, requests to `commonwealth/primary`
returned ~100 tok/s (impossible for that model, which does ~14.8 local) and the
frame-name check passed cleanly. The script has the same hole and never caught it
because it only ever ran when the primary was up.

`mesh_bench::primary_is_serving` is the check that attributes, run before **and**
after the trials, and it has to understand two hosting modes.
`ComputeRoutedProvider::resident_slots()` forwards the *in-process* engine's
view, and the in-process engine never loaded a child-hosted model — so
`resident` is `false` **forever** for a perfectly healthy child-hosted primary. A
guard reading only that field would refuse every honest run on this
configuration, which is a worse failure than the vacuous check it replaces. So
the predicate is "in-process `resident: true` **or** a `compute_children` entry
with a matching `model_id` and `lifecycle == "serving"`"; `starting` and
`warming` deliberately do not count, because those are precisely the states in
which something else answers. The canary waits on this same predicate rather than
on "I got tokens", since stopping at the first answer hands the timed trials to
whichever slot is currently covering. A run tripping any guard is still **written** — a discarded
failure teaches nobody anything, and dropping it silently makes the tool
retry-until-lucky — but `lookup` never returns it. Exit `0` valid · `1` guard
tripped · `2` bad args · `3` assertion failed · `4` nothing measurable · `5` no
daemon.

The key it files under must be the key `mesh plan` constructs, or every record is
unfindable. Both build `PlacementShard`s over **only the devices that hold
blocks** — an idle peer changes nothing about how the model decodes, and bench
has no idle device to report — and both derive the digest's `mode` from shard
topology rather than from the daemon's mode string (which has five values:
`local`, `distributed`, `child-distributed`, `stream-split`, `forming`) so the
two vocabularies cannot drift. The daemon's own word is preserved verbatim in the
record's `placement_human`.

**A record carries the pre-image of its own key** (`MeasurementRecord.witness`,
added 2026-07-30). Both digests in the key are one-way, which is right for
`lookup` — an equality test — but it meant a record could state a number without
being able to say what the number was *for*. Two runs of this fleet four hours
apart filed under different placement digests with identical `placement_human`
labels, and an exhaustive search over every integer split of the model across
both machines could not reconstruct what the earlier one described. So
`PlacementWitness` stores the exact `(mode, total_blocks, shards)` the digest was
computed from, plus a `MachineWitness` per named machine (`vram_gb`, `backend`)
because `host_hw_fingerprint: 7602642063143971880` is not something a reader can
weigh. It is *checkable* rather than asserted: `PlacementWitness::explains`
re-runs `placement_digest` over the stored fields, and `Configuration::faithful`
applies that check at the point of use, so a witness built from different inputs
than its key is treated as absent rather than quoted. Descriptions are
deliberately outside the digest — improving what a peer advertises must not
orphan every record naming it — and are deliberately **capacities and labels,
never rates**, so nothing here can be divided by anything else to resurrect the
§4.5 size-law. Not a schema bump: unlike v1→v2, whose missing field was a *key*
field, old rows still serve exact hits and are kept saying "not recorded".

**A record also carries the conditions it met** (`MeasurementRecord.conditions`,
added 2026-07-29). The witness above explains *what* a run measured; this is the
other half — the co-resident slot set, host RSS before and after, daemon uptime,
and the wall-clock span of the trials. It exists because four runs under one key
came back 7.75 / 8.38 / 8.53 / 11.08 tok/s and nothing recorded could say which
of them met a busy machine. Every field is something that can differ between two
runs of an *identical* configuration, which is exactly the class the key cannot
hold — so `RunConditions` sits beside the key and **never in it**. Keying on them
would give every run a unique unmatched key, `lookup` would never find more than
one run, and the variance the field exists to expose would become structurally
invisible (test `conditions_never_reach_the_key`). Two traps are closed by
construction: an empty slot list renders as the *finding* "nothing else resident"
rather than as silence, and a role whose `model_id` equals the primary's is not
counted — with `[models].fast` absent, `fast_path()` falls back to the primary
GGUF and `/status` reports a `fast` slot holding the measured model itself, which
filtering by role name alone would have recorded as its own co-resident. Old rows
say "conditions not recorded", never implying a quiet box. `link_rtt_ms` stays
`None`: iroh 1.0 exposes no per-peer RTT on `remote_info`, and a timed round trip
would measure the link *plus* the peer's request handling, which must not be filed
under that name. `mesh bench --history` also prints each row's abbreviated `pd2:`
key and warns when two rows share a `placement_human` under different digests —
the misreading that once produced a reported variance that was never real.

This is what makes `near_misses` load-bearing. The key pins the exact split *and*
the exact silicon, so a reader on hardware we have never seen essentially never
gets an exact hit, and `differs by: split, host-hardware` gives them nothing to
judge with. `near_misses` now returns a `Difference { facet, theirs, ours }` per
facet — `beefymac 12 · ruggedfox 36 +head` against `beefymac 24 · ruggedfox 24
+head` — rendered in both the human plan and `--json` (`differences[]`), with
`differs_by` *derived* from it so the two cannot disagree. `theirs`/`ours` are
`None` where that side kept no witness: the difference is real, and declining to
characterise it is the point. `n_ctx`, `link` and `probe_version` live in the key
itself, so even a pre-witness record reports "measured: 32768 · yours: 8192".

**Measurements travel** (2026-07-30). A measurement is worth most to the machine
that did not take it: locally it recalls what a run felt like, on a peer it
answers what a configuration *would* feel like on hardware the reader cannot try.
Records gossip under `app_id = "mesh-measurements"` as versioned `to_wire`
envelopes (a peer on another `SCHEMA_VERSION` is dropped by `from_wire`, not
half-read), keyed by `wire_key` — `{measured_at:010}/{hash}`, derived from the
record so a republish overwrites its own entry rather than accumulating copies,
and lexicographically chronological so a raw `scan` is readable. The rate enters
that hash **quantized to 0.001 tok/s** because `serde_json` is built without
`float_roundtrip`: a record passes through JSON twice (file, then wire) and can
come back one ULP off, which would otherwise let the same run compute two keys
and leave an orphan entry LWW could never overwrite.

Three constraints make travel safe rather than merely working:

- **Peer records never enter `MeasurementFile`.** `lookup` still answers only
  "what did *this* machine measure", so no peer's number can be served as the
  reader's own — `mesh plan` keeps saying "not measured **here**" and offers the
  peer's beside it. They reach the operator only through `near_misses`, carrying
  `NearMiss.taken_by` (`None` = this machine, `Some(name)` = a peer).
- **Invalid runs do not travel.** `to_wire` refuses them: a failure is glassbox
  material for the operator who caused it and noise, or worse a mis-read
  capability claim, to everyone else. `--history` still shows them locally.
- **Origin comes from the KV entry, not the payload.** A node cannot claim to be
  someone else by writing a name into bytes it controls. `ForeignRecord` pairs the
  gossip-carried origin with the record; the friendly name is resolved against
  live membership at read time, so a departed peer keeps its records and loses
  only its name.

An exact-key *peer* hit is kept (`NearMiss::is_exact`) rather than filtered as a
non-miss — someone with the same silicon, split, link and context measured the
thing being asked about, and that is the most informative record travel can
deliver. It is still never the headline.

The pipes: `svrn mesh bench` runs in the CLI, and gossip publishes from the
daemon's `MeshStore`, which is `in_memory()` — no file to open, no lock to share.
So the daemon hands over a door at `POST`/`GET /v1/mesh/measurements`
(`mesh_http.rs`, localhost-only like its siblings; `?include_self=true` is a
diagnostic that shows what this node has put on the wire). The CLI's only caller
is `mesh_travel.rs`. Disk is written *before* the wire, so `mesh bench` works with
no daemon and a failed publish reads as "not on the mesh yet", never as a lost
record. Because the buffer empties on daemon restart,
`bootstrap::republish_local_measurements` reloads the durable file at boot —
without it every node's history would evaporate from the mesh one restart at a
time while looking perfectly intact locally. Verified live: `published=3
withheld=3 total=6` at boot, and re-POSTing a record returns the same key with no
new entry. **Not** routed through `NodeCapabilities.benchmark`, which stays
`None` — that field feeds the ranked-dispatch clamp and arms the §4.5 size-law;
`gossip_never_advertises_a_benchmark` fails the build if it is populated.

The strong-peer-topology roadmap (latency-class hierarchy: cascade
routing, draft-on-spoke/verify-on-hub speculation, hub queue
discipline — each reality-checked against this codebase) is
[`docs/specs/MESH_INFERENCE.md`](./docs/specs/MESH_INFERENCE.md).

`commonwealth-inference/orchestrator/` (multi-process supervision for
the standalone-daemon topology — flagged for its own liveness
investigation in OICP_RATIONALIZATION.md):

- `ManagedProcess` tracks lifecycle states (`Starting | Running |
  Unhealthy | Failed | Stopped`); graceful SIGTERM with timeout,
  then SIGKILL.
- `HealthTracker` polls every 5s; 20-sample latency window;
  `Unresponsive` after 3 consecutive failures.
- `GracefulDeparture` — 30s countdown state machine
  (`Announced → Rebalancing → Draining → Complete`).
- `FaultDetector` collapses health changes into `FaultEvent`s.

### HTTP API

Two listeners, two trust domains.

**Client API — :9741, binds 0.0.0.0** (federated inference needs peer
reachability). Loopback callers pass free; non-loopback callers go
through a bearer-token layer (`client_auth`, `[daemon] client_token`,
with exempt paths for federation/health) — added with the SaaS
hardening, 2026-07.

| Path                          | Notes                                                  |
|-------------------------------|--------------------------------------------------------|
| `POST /v1/chat/completions`   | OpenAI-compatible. Routing differs by daemon shape (embedded vs standalone) — see `commonwealth/docs/routing-field-guide.md`. `LocalOnly` privacy → 400. |
| `POST /v1/responses`          | OpenAI Responses-API adapter (codex 0.130+). Wire-format translator over chat-completions. See [`docs/inference.md`](./docs/inference.md). |
| `GET  /v1/models`             | Loaded models w/ capabilities + performance estimates  |
| `POST /v1/embeddings`         | Embedding endpoint (what `embed_http::http_embed_fn` peers call) |
| `POST /v1/knowledge/search`   | Determines target corpora, fans out, merges, reranks   |
| `/v1/apps*`, `/app/{app_id}/{*path}` | Mesh-app install/status + reverse proxy (`commonwealth-app`) |
| `GET  /status`                | Node / mesh / inference / knowledge summary            |
| `GET  /oicp/v1/capabilities`  | Provider manifest + federation info                    |
| `/api/{version,tags,ps,show,chat,generate,embed,embeddings}` | **Ollama-native compatibility shim** (`routes_ollama.rs`). Pure translation over the OpenAI handlers above — lets Ollama-native clients (Open WebUI's Ollama mode, IDE plugins) connect. `chat`/`generate` are non-streaming-backed in v1: the inner handler runs `stream:false` and the complete answer is framed as Ollama NDJSON (one content frame + terminal). No CORS layer + the same auth posture as `/v1/*` (documented in-module); incremental streaming is a tracked follow-up. |
| `/v1/mesh/*` `/v1/admin/*` `/mcp/*` | **Loopback-only** (router middleware + per-handler `enforce_localhost`) |

**Internal API — :9742, plaintext (perimeter-trust)**

No per-request auth: the internal routes (gossip, scheduling, model/index
transfer, knowledge fan-out) trust the network boundary. Binds `0.0.0.0`
by default — set `[daemon] internal_bind` to pin it to a private interface,
or create the mesh with `require_encryption` to force all traffic onto the
iroh QUIC transport (which binds the internal router loopback-only). The
historical per-session-cert/`TrustStore` mTLS scaffolding was removed
2026-06-15 (see §5 "TLS / mesh encryption"); never describe `:9742` as mTLS.

| Path                                | Purpose                          |
|-------------------------------------|----------------------------------|
| `POST /internal/gossip`             | Gossip exchange                  |
| `POST /internal/scheduling/intent`  | Scheduling decision notification |
| `POST /internal/scheduling/plan`    | New shard plan distribution      |
| `POST /internal/model/transfer`     | Model file transfer (peer-to-peer) |
| `POST /internal/rpc-warm`           | Distributed inference: host asks a worker to seed its RPC tensor-cache shard before a distributed load (auto-warm). `serve_model_file` honors `Range` for shard-only fetch. The host distributes only to ELIGIBLE workers (`sovereign-mesh::worker_eligibility` — settle + flap-quarantine, surfaced in `svrn mesh status`); a remote crash mid-compute `GGML_ABORT`s the host, so distributed inference requires host supervision. See `docs/RPC_DISTRIBUTED_INFERENCE.md`. |
| `POST /internal/index/transfer`     | Corpus shard upload (push)       |
| `GET  /internal/index/serve`        | Corpus shard download (pull)     |
| `POST /internal/knowledge/search`   | Inter-node shard query (fan-out target) |
| `GET  /internal/latency/probe`      | Latency probe response           |

The table shows the core mesh-protocol routes. `server.rs`'s
`internal_router` registers ~30 more operational routes (corpus
lifecycle `/internal/corpus/*`, model load/unload/inventory, app state
+ registry, budget / quiesce / foreground-state controls, and the
contribution/activity family listed under §5 "Desktop
production-readiness").

The loopback guard is defended in three layers: router-level
`from_fn(loopback_only)` middleware, per-handler `ConnectInfo`
extraction, and a pinned listener-shape test
(`admin_http::tests::loopback_guard_works_under_production_listener_shape`).
The listener must use
`.into_make_service_with_connect_info::<SocketAddr>()` in
`daemon::start_daemon` — bare `axum::serve` leaves `ConnectInfo`
absent and the guards fail closed for *every* caller.

### Knowledge, ledger, peer prefs

- `MeshCorpusManager` / `ShardManager` — install / list / remove /
  shard prepare / install received / consolidate.
- `embed_http::http_embed_fn` — POSTs to `/v1/embeddings` so a node
  without a local embed model still ingests via the engine.
- `grounding.rs` — `GroundingConfig` + `search_for_grounding` +
  `format_knowledge_context`.
- **Dimensional contribution ledger**
  (`commonwealth-core::contributions`) — append-only event log
  (`LedgerEventKind` variants `InferenceServed`, `InferenceReceived`,
  `KnowledgeQueryServed`, `ShardTransferred`, `StorageSnapshot`)
  with pure aggregation into per-node `NodeContributions`. No
  `balance`, no exchange rate, no ranking — units are
  incommensurable. Storage in
  `commonwealth-state::ContributionEmitter` (gossip-replicated
  `MeshStore` under `app_id = "contributions"`). Pull-side
  `ShardTransferred` is emitted by the merge leader on behalf of
  the peer that shipped bytes — the schema carries an explicit
  `from_node`, and the aggregator credits `bytes_served` to it.
- **Local Activity ledger** (`commonwealth-core::activity`) — the
  glassbox counterpart answering "what is *my* daemon doing, even as
  a mesh of one?" A sibling of the contribution ledger, not part of
  it: `ActivityEventKind` variants (`LocalInferenceServed`,
  `EmbeddingsServed`, `LocalKnowledgeServed`, `ChunksIngested`,
  `CorpusEnriched`, `NewsworthyFetched`) record resource work that
  never crosses a peer boundary, and `aggregate_activity` folds them
  into one self-view `ActivitySummary`. Storage in
  `commonwealth-state::ActivityEmitter` under the **gossip-excluded**
  `app_id = "activity-private"` (in `GOSSIP_EXCLUDED_APP_IDS` — your
  own usage never gossips, the deliberate contrast to
  `contributions`). Recorded at daemon boundaries: the local arm of
  `routes_inference::chat_completions`, the `embeddings` handler
  (previously unrecorded — a peer using your embed model was
  invisible), and the corpus-ingest `ProgressCallback`
  (`ChunksIngested` on `Complete`, `CorpusEnriched` on the
  structural-atlas pass). Surfaced via `GET /internal/activity/
  {summary,recent}`. Desktop chat runs the in-process Runtime and
  never hits a daemon HTTP boundary, so its slice is read *derived*
  from the `ResponseProvenance` already persisted on each message via
  `SqliteStateStore::summarize_chat_activity` (no new write path).
  All three feed Settings → **Activity & Sharing** (rebuilt
  `SharingSection.svelte`), which also hosts "the reins": ingest
  throttle, mesh-quiesce, and peer-share ceiling/pause controls.
- **Peer preferences (Ostrom sanctions)** —
  `commonwealth-state::peer_preferences` is the local-only,
  gossip-excluded store of per-peer affinity multipliers (clamped
  to `(0.0, 1.0]`). The manifest endpoint reads `X-Node-Id` and
  multiplies advertised `CapabilityClaim.affinity` per peer; the
  penalized peer's scorer sees lower affinities and naturally
  routes elsewhere. Filtering enforced in two places
  (`peer_preferences.rs` + `store.rs`).
- See [`docs/MESH_LOAD_AWARENESS.md`](./docs/MESH_LOAD_AWARENESS.md)
  for peer-admission, contribution ceiling, and foreground-yield.

### Test harness

`commonwealth-test-harness`:

- `SimulatedMesh` — orchestrates many `SimulatedNode`s in-process,
  each with its own `AppState` and HTTP listeners on random ports.
- `SimulatedNodeBuilder` — fluent hardware-profile builder.
- `MockLlamaServer` — Axum responding to `/v1/chat/completions` and
  `/health` with canned responses; request counting via
  `Arc<AtomicU64>`.
- `fixtures.rs` — reusable hardware profiles, models, capability
  profiles.

`tests/integration.rs` covers mesh formation, gossip convergence,
layer assignment, inference E2E through the mock server, fault
recovery, graceful pause/resume, OICP routing, multi-model
portfolio, knowledge fan-out, ledger accuracy. Deterministic timing
— no real 10s gossip waits.

### Distributed state + apps

`commonwealth-state::MeshStore` — gossip-replicated SQLite KV (WAL
mode): `StoreEntry { app_id, key, value: Bytes, timestamp, origin:
NodeId }`, LWW conflict resolution, per-`app_id` namespace,
`RetentionGc` for TTL.

`commonwealth-app` — mesh app platform: `MeshAppManifest`
(gossiped), `AppPermissions` (`mesh_store_read`/`_write`,
`inference_access`, `knowledge_access`), `AppRegistry`,
`AppProcess` lifecycle, `AppPortMap` + `forward()` reverse-proxy.

**Mesh-replicated workspace**. The `alignment` family replicates a
working set of files (default `~/.claude/`) across mesh peers
without a central server. Newest-mtime-wins via `merge_shards`'s
`mutable_merge = "source_doc_id_newest_mtime"` policy. Projector
under exclusive lock; mtime-stable. CLI: `svrn alignment`.
Corpus bytes are local-only (mutually-authenticated peers only —
not gossiped onto the open mesh).

### CLI

```
commonwealth init --name "..."          Create a mesh, get a join key
commonwealth status                     Node + mesh state (GET /status)
commonwealth balance                    Contribution ledger (local store)
commonwealth models                     Models advertised (GET /v1/models)
commonwealth corpus status              Ingestion/shard status (GET /internal/corpus/status)
commonwealth corpus collaborate <id>    Recruit peers for a mid-flight ingestion
commonwealth daemon start               Run the daemon
commonwealth recipe test/validate       Community-recipe harness
commonwealth peer-preference …          Per-peer affinity (local-only)
```

Every command does real work (2026-07-01): the aspirational
placeholders that printed `(In production, this would …)` and exited 0
were removed, and `status`/`models`/`corpus status` were implemented as
thin views over the HTTP control plane. Mesh lifecycle UX (create /
join / rotate / status across nodes) lives under `svrn mesh`;
daemon lifecycle under `svrn daemon`.

### Deployment

`contrib/`: `install.sh` (curl installer),
`systemd/commonwealth.service`,
`launchd/com.commonwealth.daemon.plist`.

### Desktop production-readiness (W1–W6)

A coordinated stack supporting the friends-and-family launch.
Failure modes addressed: daemon crash drops the whole UI, peer
work pins the GPU while the user is chatting. Components:

- **W1 — child-process daemon supervisor**
  (`sovereign-desktop/src-tauri/src/supervisor.rs`) — Tauri-free,
  broadcast-driven, heartbeat, exponential backoff
  (1s→5s→30s→2min), crash-loop ceiling, bounded stderr buffer,
  crash-log persistence to `<data_dir>/crash-logs/`. The
  implementation actually lives in `sovereign-compute/src/supervisor.rs`
  and is re-exported here (moved 2026-07-20) so the daemon's
  compute-child manager shares one state machine.
  **The crash-loop ceiling counts CONSECUTIVE crashes and resets only
  on proof — a generation that stayed healthy for
  `healthy_reset_after` (60s in both production configs). It is not a
  sliding wall-clock window** (changed 2026-08-03): a window is
  unreachable for any child whose spawn→crash cycle is longer than the
  window is wide, which is exactly what let a 148 GB distributed
  primary — 4m36s to load, 13s serving, dead — respawn without limit
  until amdgpu ran out of GPU address space and took the desktop with
  it. Backoff is additionally floored at the previous generation's
  measured load time, so an expensive child can never re-enter a load
  back-to-back. **Default ON
  since the 2026-07-18 flip** (DAEMON_RESILIENCE.md P0.1):
  `supervisor_setup.rs` spawns **this very desktop binary** as
  `current_exe() --daemon-child` — the argv arm calls
  `sovereign_cli_daemon::daemon_child_main()` (the crate is bin+lib),
  so the child is the REAL daemon with all its defenses and zero
  sidecar bytes in the bundle. Opt-outs: `SOVEREIGN_USE_SUPERVISOR=0`
  (kill-switch back to in-process `EmbeddedDaemon`) and
  `SOVEREIGN_FORCE_LOCAL=1` ("this process runs the weights" — the
  real-mode harnesses). Falling back to in-process is surfaced via the
  `supervisor-fallback` event (rendered by ReconnectBanner), never
  silent; `supervisor_reconnect` / `supervisor_active` commands back
  the banner's Reconnect button. The child-process boundary makes
  "daemon crashed → click Reconnect" a recoverable UI state instead
  of a dead window. Motivated by ggml/llama.cpp SIGSEGVs an
  in-process supervisor can't catch. First-session coverage: both
  wizard completion paths finish by mirroring the config and
  relaunching the app (`maybe_restart_into_supervised` — the wizard
  session never binds `:9741`), so a fresh install is supervised from
  its first post-wizard minute; `SOVEREIGN_FORCE_LOCAL=1` and the
  kill-switch keep the legacy in-process completion for harnesses.
- **W2 — peer-admission middleware**
  (`commonwealth-api/admission.rs`) — applied to client-port
  `/v1/chat/completions` + internal-port
  `/v1/knowledge/search`. Local requests admit unconditionally;
  peer requests are rejected with 503 + `Retry-After` when paused,
  yielding to a recent local foreground request, or refused by the
  fair scheduler. The flat ceiling became a **`commonwealth_core::fair_sched::SchedCore<NodeId>`**
  (`AppStateInner.peer_sched`): a runtime-mutable global ceiling
  (`set_slots`, `0` = reject all) **plus a per-node concurrency cap**
  so one peer can't hog the pool, **reciprocity-scaled** — a
  contributor's effective cap rises toward the ceiling, read from a
  cached per-node weight table (`reciprocity_weights`, refreshed
  ~30 s by a daemon loop from the contribution ledger). This is the
  host-side convergence point for a shared-model fleet (every
  consumer's turn lands here as a peer request keyed on `X-Node-Id`).
  `PeerInflightGuard` is RAII (`release`s the node's slot on drop,
  accurate under panic unwind). The **same `SchedCore` policy** backs
  the chat server's turn scheduler (`sovereign-server/scheduler.rs`),
  so both admission gates are fair by identical rules.
- **W3 — tray status chip + pause submenu**
  (`sovereign-desktop/src-tauri/src/tray.rs`).
- **W4 — first-mesh-join consent** —
  `DesktopConfig.first_mesh_consent`; ConsentGate renders when
  unset.
- **W6 — self-service support surface.** Built for onboarding
  non-developers who must be debugged remotely from artifacts they
  can produce unaided. Three layers, in the order a person hits them:
  - **Fix it yourself** — `health.rs` runs seven checks (`engine`,
    `model`, `mesh`, `mesh_peers`, `knowledge`, `disk`, `stability`)
    over a `HealthFacts` struct gathered by `commands/diagnostics.rs`.
    `evaluate` is pure; every non-OK check carries a terminal-free
    `fix_hint`; an unreachable probe renders `Unknown`, never a
    fabricated verdict. Rendered by `HealthPanel.svelte` at the top of
    Settings → Diagnostics, and reachable from the reconnect banner's
    **Check my setup** (via the `settingsNav` store, which App.svelte
    refuses mid-setup).
  - **Report the machine** — `prepare_diagnostic_report` writes
    `~/Desktop/svrnmesh-<reason>-<ts>.md` for any `ReportReason`, not
    only a crash. `ReportReason::parse` degrades unknown → `Other`: a
    user trying to report a problem is never blocked by an enum.
  - **Report one answer** — `turn_report.rs` + `prepare_answer_report`,
    for the complaint machine state cannot explain. The snapshot comes
    from the **frontend**, assembled from the assistant message's
    persisted metadata (route, sources, backend/peer, gate action),
    because `TurnProvenance` holds only the newest turn of a
    conversation, in memory, on one register. Each report carries a
    speakable `reference_code` derived from `message_id` via a **pinned**
    FNV-1a — a wire format, not an implementation detail: change it and
    a user's screenshot stops matching their own report file. Passage
    *text* is opt-in per report, defaulted off, and `render_turn_section`
    enforces the gate itself rather than trusting the caller.

  Every report is a file on the Desktop the user reads before sending.
  No auto-upload anywhere, and the report **states its own contents** —
  the disclosure text is derived from what is actually in the file, so
  a state-only report, an answer report, and an answer report with
  source text each describe themselves honestly. User-facing doc:
  [`docs/HAVING_TROUBLE.md`](./docs/HAVING_TROUBLE.md) (no terminal);
  the maintainer-facing `docs/TROUBLESHOOTING.md` points at it.

**MeshApp bridge (first-party sandboxed apps).** A mesh app runs in a
`meshapp-<id>` webview reached only through a permission-gated bridge.
`src/meshapp.rs` owns authorization — the app id is derived from the
host-set webview LABEL (unspoofable from JS) and checked fail-closed by
`authorize` against the granted subset in `DesktopConfig.meshapp_installs`.
`src/commands/meshapp.rs` exposes the `meshapp_*` commands: deterministic,
read-only `read_corpus` / `parcel_analytics` (reusing corpus-engine's
`compute_aggregates`, so no model originates a figure on the desktop
surface either); the graph-explorer family `graph` / `node` / `findings` /
`search_entities` / `reconciliation` / `subgraph` (top-degree nodes + induced
edges, for a node-link map) / `corpus_stats` (scale/provenance counts) /
`timeline` (documents bucketed by month, parsed from the `Date:` header every
email chunk carries) / `read_chunk`; host-only install management; and
`meshapp_open` (`WebviewWindowBuilder` + the
`meshapp_shim.js` `window.meshApp` shim over `__TAURI_INTERNALS__` + a
per-window strict CSP set in `on_web_resource_request`). The graph ops'
LOGIC lives in the **`sovereign-meshapp`** library crate (pure path-in /
DTO-out, Tauri-free) so the desktop host and the `svrn meshapp dev`
CLI server share one source of truth; the Tauri commands are thin wrappers
(permission gate + resolve the corpus's on-disk index). The ops are
**backend-agnostic**: `load_graph` dispatches on what the index
carries — a deterministic `investigation/` graph (UAP) or an `atlas/`
enrichment (Enron), projecting both into one DTO contract
(`GraphNodeDto` / `EdgeDto` / `NodeDetailDto`). The atlas adapter maps
Entity atoms → nodes and Relation/Event atoms → cited edges, resolving
each `sec_NNNNN` evidence id to a `chunks.lance` row via `chapters.json`
so `read_chunk` dereferences the source document unchanged;
`reconciliation` surfaces the cross-origin merge log as the identity
glassbox. Six first-party apps ship on this surface: SF-LVT
(`public/meshapp/lvt/`, deterministic parcel compute), UAP Blue Book
(`public/meshapp/uap/`, investigation graph), Enron
(`public/meshapp/enron/`, story-first atlas experience), **Wrapped**
(`public/meshapp/wrapped/`, a story-card show over the user's own
`conversations-anthropic` corpus), Federalist
(`public/meshapp/federalist/`, the copyable complete example), and Atlas
Explorer (`public/meshapp/explorer/`, a generic corpus-bound atlas
explorer). Wrapped's op is different in kind: `wrapped_artifact` serves
a **precomputed artifact**, never live inference —
`sovereign-meshapp/src/wrapped.rs` folds every figure deterministically,
runs a **verbatim-citation audit** (`verify_wrapped_artifact`: every
cited chunk id must resolve, every embedded quote must be a verbatim
substring of its chunk — a failing artifact is never served), and caches
`<index>/wrapped/all-time.json` keyed on the corpus fingerprint (opening
the app rebuilds a stale artifact on demand). Cards are typed; absent
data ⇒ absent card; unknown card types are SKIPPED — the forward-compat
seam future enriched cards ship through. The **v3 deck** is Scale,
Rhythm, Recurring, Turn, Obsessions, Night Shift, Cast, Door; the folds
that need enrichment or geometry live in `wrapped/semantic.rs`, the rest
in `wrapped.rs`. Three things about it are load-bearing and expensive to
rediscover. (1) **Themes come from RAPTOR `primary_entities`, not
GLiNER** — measured on `conversations-anthropic`, GLiNER's top of archive
is `People (77) · WORK (53) · Companies (46)` where RAPTOR's is
`San Francisco (37) · Federal Reserve (33) · Taoism (13)`: nouns versus a
life. `ThemeIndex` is source-agnostic (`from_enrichment | from_ner`) so an
un-enriched corpus still gets a deck, at lower quality. Themes rank by
z-scored log-odds against the archive baseline (Monroe et al.), never by
frequency — frequency ranks the baseline and returns the same list every
quarter, which is what made v2 read as topical co-occurrence. (2)
**`ConvDoc::turns` is the PARSED SUBSET of a conversation, not its
shape.** A chunk yields turns only where its text carries a
`### [ts] role` header, and 13,373 of this archive's 16,404 chunks do not
— they are mid-answer continuation fragments that begin mid-sentence.
Anything reasoning about conversation SHAPE must read
`ConvDoc::chunk_ids`; `turns` is for quotes and clocks. Reading shape off
`turns` cost the Turn card 90% of its evidence (1,510 of 15,283 seams,
135 of 425 conversations) until 2026-07-26. The corollary is a licence,
not just a warning: because an unparsed chunk provably holds no turn
boundary, "the last thing you said before the seam" is correct at any
chunk distance, so the quote walk is deliberately unbounded. The same
blind spot ran through TEXT until 2026-07-26: a `parse_turns` block
stops at its chunk's edge, so a turn's words have to be walked forward
across the continuation chunks it spills into (`continuation_lead` +
`build_conv_docs`). Counting header-bearing text alone saw 19.9% of the
archive and reported 704,924 words at a 2.7x assistant:user ratio where
the truth is 3,512,842 at 14.9x. (3) **The archive stamps UTC, and the
deck shows one clock — the reader's.** `semantic::LocalClock` is
inferred once per build (`infer_utc_offset` locates the 4h trough in the
user-turn histogram and places its centre at 03:00 local; this archive
infers UTC-7) and handed to every card that shows an hour: the Rhythm
heatmap shifts whole datetimes, weekday included, and the Night Shift
bands read the same offset. Two cards inferring it separately is two
chances to disagree in front of the reader — which is exactly what
shipped in v3, where the grid peaked at 20:00 UTC while Night Shift
called those same turns 13:00. `WRAPPED_SCHEMA_VERSION` (now 4) is the
lever that forces a cached artifact to rebuild when a fold change like
this must reach existing installs before the corpus next updates. Bundles compose the
**MeshApp SDK**
(`public/meshapp/_sdk/`, dependency-free ES modules served under the CSP): a
corpus-bound `connect()` bridge client, CSP-safe DOM helpers, and the reusable
views (force-graph, timeline, reconciliation reveal, entity-detail, cited-edge,
search, scale-banner, and Wrapped's `storyShow` full-screen card shell +
`heatGrid` hour-of-week view in `story.js`) + `meshapp.css` — Enron's bundle
is ~150 lines of
composition, not ~600 of hand-rolled DOM. Each bundle carries a self-describing
`meshapp.json` manifest (id/name/corpus/grants/entry/trust — the unit a registry
distributes); `scripts/gen-meshapp-catalog.mjs` (pre{dev,build}) aggregates them
into `meshapp/catalog.json`, and `MeshAppsSection` discovers apps from it via
`loadCatalog()` rather than a hard-coded list. So adding an app is a bundle + a
manifest (+ an atlas reader only when the backend differs) — no host code edit.
**Local dev loop:** `svrn meshapp dev <id>` (sovereign-cli-llm) serves a
bundle + its `_sdk/` and injects a `window.meshApp` that proxies the explorer
ops over HTTP to the same `sovereign-meshapp` functions, reading a local corpus
index — so a bundle is iterable against real data without the desktop;
`svrn meshapp new <id> --corpus <c>` scaffolds one. **Corpus as a managed
dependency:** a manifest's `corpus_data` (size + the recipe the bundle ships,
carrying a `[prebuilt]` HF block) makes the corpus first-class — `MeshAppsSection`
shows its presence and, when missing, a one-click **"Get data (N GB) & Open"** that
stages the recipe (`meshapp_stage_corpus_recipe` → `~/.sovereign/recipes/`) and runs
the existing prebuilt install with a progress bar. **Curated registry:** `svrn
meshapp publish/install/list` (sovereign-cli-llm `meshapp_registry.rs`) distribute an
app as a self-contained `tar.zst` (bundle + a copy of `_sdk/`); install verifies the
artifact's sha256 (refuses tampering) and unpacks under `~/.sovereign/meshapps/<id>/`.
TRUST = integrity (sha256) + curation (membership in the reviewed
`sovereign-recipes/meshapp-registry.toml`); `meshapp dev` runs installed apps. The host
enumerates them via `meshapp_installed_apps()`; in-window opening of an installed app
(serving it from the install dir via a `meshapp://` scheme) is the remaining
integration. End-to-end runbooks: `docs/MESHAPP_CONSUMER.md` (replicate a demo) and
`docs/MESHAPP_AUTHORING.md` (recipe → corpus → app → publish). **Isolation caveat:**
Tauri v2 does not gate app
commands per-window (tauri#9227) — a webview with IPC can invoke any
registered command — so `capabilities/meshapp.json` only narrows the
core/plugin surface; true isolation for UNTRUSTED third-party apps needs a
no-IPC bridge (custom protocol / postMessage), a deferred platform
milestone. The bundles are verified headlessly by
`tests/e2e/specs/meshapp-{lvt,uap,enron}.spec.ts` (Playwright, a11y
locators), each mocking `window.meshApp` + one real-shim→IPC wiring test.

**Accessibility tooling.** `npm run a11y` (`tests/e2e/scripts/a11y-report.mjs`)
is a dev-runnable, NON-BLOCKING axe-core scan of the chat surface + the
mesh-app bundles, writing a readable report to `test-artifacts/a11y/`
(glassbox insight into a11y shortcomings; no CI gate). Two reusable a11y
seams live in `packages/chat-ui` and are shared by desktop + mobile:
`completionAnnouncement` (per-turn screen-reader wording for the polite
completion live region — announce on completion, never per token) and the
`use:dialogFocus` action (modal focus-trap + focus-restore-on-close,
adopted by `MeshJoinDialog`/`MeshSettings`/`NewProjectDialog`/
`DocumentInspector`/`EchoOverlay`). Dynamic a11y behaviours (live regions,
focus restore) are verified by manual screen-reader testing, not axe.

Control routes (loopback-only, on the internal port :9742):
`GET /internal/contribution/status`,
`POST /internal/contribution/ceiling`,
`POST /internal/contribution/pause`,
`POST /internal/contribution/resume`,
`GET /internal/contribution/recent`,
`GET /internal/activity/summary`,
`GET /internal/activity/recent`.

Open polish: tray icon tint, HintCues nudge to Sharing tab, removing
the in-process `EmbeddedDaemon` fallback entirely (the default-flip
itself landed 2026-07-18 — the fallback remains as a surfaced degraded
mode), and graceful SIGTERM-with-grace on daemon shutdown. Daemon-side
resilience roadmap:
[`docs/specs/DAEMON_RESILIENCE.md`](./docs/specs/DAEMON_RESILIENCE.md).

**W7 — live-turn re-attach (streaming survives a conversation switch).**
`chat.machine` owns exactly ONE conversation's `messages` +
`streamingMessageId` and wipes the latter on every `HYDRATE`
(conversation switch), so a turn the user navigated away from was
orphaned: the `conversation_id`-tagged `message-chunk` /
`message-complete` / `message-error` events were dropped by the
`messageId` guard, and the backend persists the assistant row only
AFTER the stream ends (`StreamHandle` contract) — so on return there
was no row, no loading affordance, and the answer never landed. Most
visible on a slow turn whose synthesis is offloaded to a mesh peer
(minutes-long, long enough to switch away). Fix: a runed singleton
registry `stores/liveTurns.svelte.ts`, fed by the global stream
listeners keyed on `conversation_id` regardless of which conversation
is on screen; `ChatView.loadConversation` re-attaches on return
(`REATTACH_STREAM` restores the affordance + partial text for an
in-flight turn; a terminal turn renders its answer/error). Scope: lives
while ChatView is mounted (survives conversation switches, NOT app
restart / Settings-Atlas unmount — that durability belongs to a
persisted streaming placeholder row, deferred). `message-error` now
carries `{conversation_id, message_id}` (`commands/chat.rs`
`MessageErrorPayload`) so a failed backgrounded turn is attributable.
Pinned by `tests/e2e/specs/chat-orphaned-turn.spec.ts` +
`stores/liveTurns.test.ts`.

### Pinned worker pods as inference peers

Ephemeral worker pods (Vast L40S rented via `pipeline pod up`)
join the mesh scheduler's inference pool as one more peer, scored
by the same OICP load balancer. Pods aren't gossiped — owner-
private, TLS-pinned, authenticated by Ed25519 `WorkerToken`. See
[`docs/PINNED_WORKER_AS_INFERENCE_PEER.md`](./docs/PINNED_WORKER_AS_INFERENCE_PEER.md)
and [`docs/EPHEMERAL_WORKER_PODS.md`](./docs/EPHEMERAL_WORKER_PODS.md).

---

## 6. How the four projects fit together

**Sovereign standalone** — Tauri / CLI / server runs against
`EmbeddedLlamaCpp`. Knowledge via `MeshCorpusManager` (named for
the mesh case but works without one). `EmbedFn` wraps the local
Embed slot.

**cmnwlth standalone** — daemon serving `localhost:9741`.
Any OpenAI-compatible client points at it. Knowledge ingest uses
`embed_http::http_embed_fn` so a node without a local embed model
still indexes via the engine.

**Sovereign + cmnwlth (integrated)** —
`sovereign-mesh::EmbeddedDaemon` runs cmnwlth in-process.
Runtime inference is wrapped in `MeshInferenceProvider`, which
OICP-routes synthesis to peers when scoring favours them. Both
sides share `sovereign_mesh::oicp_select` so Joiner's selected
model and Founder's served slot can't drift.
`complete_stream_with_id` returns model attribution alongside the
stream so peer-served completions show in
`ResponseProvenance.inference_backend` as
`"Qwen3.5-9B.Q8_0 @ peer mac-peer"`. Skills with
`privacy = "local_only"` short-circuit to local.

**Desktop attach mode** — both the desktop app and `svrn
daemon` want :9741. The desktop probes
`http://127.0.0.1:9741/v1/models` at startup (`bootstrap::detect`);
on success it enters Attach mode: inference flows through
`RemoteApiProvider`, mesh mutations go over HTTP via
`sovereign-mesh::mesh_http`, and `commands::save_config` POSTs
`/v1/admin/reload` so the daemon swaps its `InferenceProvider` in
place. Smoke test at `sovereign/scripts/smoke-attach-mode.sh`.

`/v1/admin/reload` rebuilds only what changed:

| Changed field                           | Reload action                              |
|-----------------------------------------|--------------------------------------------|
| `models.primary` / `.fast` / `.embed`   | Rebuild via `ProviderFactory`, atomic swap |
| `daemon.client_port` / `.internal_port` | `restart_required: true`                   |
| `daemon.client_bind` / `.client_token`  | `restart_required: true`                   |
| `data.dir`                              | `restart_required: true`                   |

When `restart_required: true`, `save_config` falls back to
`launchctl kickstart -k gui/$(id -u)/com.sovereign.daemon` (macOS)
or `systemctl --user restart sovereign` (Linux).

---

## 7. Build, test, run

### Prerequisites

- Rust toolchain (stable)
- `cmake` (llama.cpp)
- `protoc` (LanceDB → `lance-table`); macOS: `brew install
  protobuf`; Debian: `apt install protobuf-compiler`
- For cmnwlth: `llama-server` + `rpc-server` from
  `llama.cpp` on `PATH`
- For desktop: Node.js + Tauri 2
  (`cargo install tauri-cli --version "^2"`)

### Build / test

The repo is **one unified Cargo workspace** — every crate a member under the
root `Cargo.toml` (`sovereign/`, `commonwealth/`, `corpus-engine` + its
carve-outs are directories of member crates, **not** separate workspaces). Use the
**sovereign watcher** (`lint_status` / `test_status` MCP tools) for
compilation feedback — running `cargo build` / `cargo test` directly via Bash
contends with the watcher for the file lock and idles.

**Watcher liveness is heartbeat-driven and self-healing.** The
`WatcherCoordinator` loop stamps a shared `WatcherHeartbeat`
(`corpus-engine-watchers/src/watcher_coordinator.rs`) every iteration;
the status tools read it through `code/watcher_health.rs`. Every
`lint_status`/`test_status`/`build` response carries a `watcher`
object — `{live, reason, configured, heartbeat_age_secs, hint}`. When
`live` is false the result is *orphaned* and `status` is reported as
`watcher_down` (never `fresh_*`), so a stale run can't masquerade as
current — the failure mode behind "the watcher silently goes stale."
A daemon-side `WatcherSupervisor`
(`sovereign-cli-daemon/src/watcher_supervisor.rs`) owns the coordinator
and restarts it (bounded backoff) when the loop task dies or its
heartbeat freezes; `svrn doctor`'s `watcher_live` check probes the
same signal, catching configured-but-dead — which a config-presence
check cannot. If the runner sections are commented out in
`.sovereign/sovereign.toml`, restore from
`.sovereign/sovereign.toml.with-watchers`.

```sh
# One workspace — build / check / test everything from the repo root:
cargo build --release --workspace          # bundled assets copied via build.rs
# For LOCAL deployed-daemon iteration use scripts/dev-release.sh instead of
# plain --release: same opt-level, but LTO/CGU=1 overridden via env — a
# one-line change costs seconds instead of ~7.5 minutes. (A custom cargo
# profile can't do this: llama-cpp-sys-4's build script panics under any
# custom profile — see the script header.)
cargo check  --workspace --all-targets      # what CI's `check` job runs
# The user-facing CLI spans 4 binaries (dispatcher + 3 siblings) — rebuild
# all of them (editing one + rebuilding only the dispatcher is a silent no-op):
cargo build --release -p sovereign-cli -p sovereign-cli-daemon \
            -p sovereign-cli-dev -p sovereign-cli-llm
```

```sh
cargo test --workspace                      # no GPU / network / model weights (§12.4)
```

No tests require GPU, models, or network. Sovereign uses
`DeterministicInference` + in-memory SQLite + real FTS5 for
functional tests. cmnwlth's harness runs simulated meshes
deterministically.

**Regression gate — `scripts/sovereign-test.sh`.** The same
`cargo test --workspace` surface the watcher polls, wrapped for on-demand
use: it pipes cargo through `sovereign-cargo-test-adapter` (Tier 2 JSONL)
and, with `--human`, prints a compact pass/fail summary + failing-test list.
Aggregation is a *single* pass over the adapter JSONL — it reads the
authoritative counts from the adapter's trailing `summary` record rather
than re-parsing every line (the prior per-line `python3` fork storm cost
minutes on a ~7.7k-test run). Three scoping levers, with **different reach**:
- `--package <name>` → `cargo test -p <name>`: scopes BUILD **and** RUN to
  that crate + its dep graph. The real lean lever.
- `--changed` → auto-maps git-changed `.rs`/`Cargo.toml` (vs HEAD + untracked)
  to their owning crates (nearest-ancestor `[package]` manifest) and unions
  them into `-p` — "just the crates you touched." Non-crate paths (scripts/,
  root virtual manifest) resolve to nothing → loud fall-back to full
  `--workspace`, so the gate never silently under-covers.
- `--filter <pat>` → a libtest **name** filter (`cargo test … -- <pat>`):
  narrows which tests RUN within the selected scope, but a name filter cannot
  shrink the compile on its own, so as of 2026-07-24 it **also scopes the
  build**: a single test cost 36s workspace-scoped against 1s for
  `cargo test -p <crate>`. The scope is derived from the pattern — libtest
  matches it as a substring of each test's full path, so any test it can match
  must have that substring in its own crate's sources, and `git grep`-ing for
  it OVER-approximates the owning crates (it can select a crate with no
  matching test, harmless; it cannot miss one that has a match). Verified on a
  deliberately broad 29-crate pattern: identical 187-test set vs workspace
  scope. Broad patterns degrade gracefully to the full workspace;
  `--filter-workspace` forces the old compile-everything behaviour.

Feature flags are **scope-aware**. `--features <pkg>/<feat>` is a hard cargo
ERROR when `<pkg>` is outside the `-p` selection, so the previously
unconditional `-F corpus-engine/treesitter` made `--package <leaf-crate>` fail
outright (`--package oicp-types` → exit 101, zero tests run). `resolve_features`
in `scripts/lib/cargo-scope.sh` now emits a flag only when its package is in
the selection's workspace-internal dependency closure; the unscoped run still
gets both flags, so the gate's own coverage is unchanged. That lib also holds
`crate_for_path` + `keep_members`, shared with `nextest.sh` so the two runners
cannot drift apart — and `keep_members` is load-bearing: `crate_for_path` can
resolve a `[package]` dir that is **not** a workspace member (`sovereign-mobile`
is a standalone Tauri app), and `cargo -p <non-member>` aborts the entire run.

Scoped runs share the workspace `target/` unless sccache is genuinely wired
(`command -v sccache` **and** `RUSTC_WRAPPER`). The isolated
`target/sovereign-test-scoped` dir only pays off when sccache serves the
unchanged crates; without it it is just a second permanently-cold build tree —
it had reached 37 GB and a week of staleness before this gate landed. The
fallback re-accepts treesitter feature-unification thrash (a bare `-p` flip can
rebuild `corpus-engine` + its dependents), which is far cheaper than a
guaranteed from-scratch build.

**Executors — `--engine auto|nextest|cargo`.** `cargo test` runs the
workspace's 178 test binaries *serially*: 90.5s of in-binary time against a
16.7s slowest binary. nextest runs them in parallel, so the gate's own
`--engine nextest` (the `auto` default wherever cargo-nextest is installed)
cuts test execution to ~19s. `auto` falls back to cargo on a machine that never
ran bootstrap — nextest is a speed win, not a correctness dependency — but an
*explicit* `--engine nextest` errors rather than silently downgrading, since
quietly running a different executor is how "nextest is green" comes to mean
nothing. `scripts/bootstrap.sh` installs cargo-nextest at a **pinned** version
(profiles live in `.config/nextest.toml`, so version skew is silent behaviour
skew across the mesh).

Switching engines changes the clock, never the coverage. Three things enforce
that. (1) nextest reports via JUnit rather than libtest stdout, so
`sovereign-nextest-junit-adapter` translates it into the **same** Tier 2 JSONL
the libtest adapter emits — `n` stays the bare test path, so no existing
consumer can tell the engines apart. (2) nextest cannot run doctests at all, so
the gate unconditionally appends a `cargo test --doc` pass (~4s; the workspace
has 43 doctest targets and 0 runnable doctests today, making it pure insurance
against the first doctest anyone writes silently never running). (3) The JUnit
report is written at the *end* of a run, so a run that dies during compilation
would leave the previous report in place — the gate deletes it first, making
"no report" unambiguously mean "this run produced no results" instead of
replaying a stale green. `--no-tests=pass` restores cargo's exit-0-on-no-match
semantics, which `--filter`'s deliberate over-approximation depends on.

`scripts/nextest.sh` remains as the fast-path dev runner, with nextest's richer
`-E` filter-expression language; the gate deliberately keeps plain substring
filter semantics so its results are comparable across both engines.

**Where the gate runs — `scripts/pre-push.sh` first, CI second.** As of
2026-07-24 the *primary* correctness gate is a pre-push hook, not GitHub
Actions. The reason is a failure mode metered gates have and unmetered ones do
not: on 2026-07-24 the repo exhausted its Actions allowance and every job began
aborting in ~4s with "the job was not started because recent account payments
have failed" — which on a PR page is nearly indistinguishable from a gate that
ran and passed. Audited spend was ~6,600 billed min/month against a 3,000-min
allowance, 56% of it CI, with the Actions cache measured **empty** (so every run
cold-built the workspace incl. llama.cpp: 56.7 min median). The full audit,
the five mechanisms behind it, and the resulting budget are in
`docs/CI_ECONOMY.md`; `scripts/ci-spend-audit.sh` reproduces the numbers on
demand (GitHub's own `/timing` endpoint reports zeros here, so the script sums
per-job wall time and applies runner multipliers itself).

The hook runs this same regression gate, scoped to what the push changes, plus
rustfmt, `xtask docs-gate`, and the desktop `npm run check`/`test` pair. It is
installed via `scripts/install-git-hooks.sh` (called from
`scripts/bootstrap.sh`), which points `core.hooksPath` at the
version-controlled `.githooks/` — so the gate is a reviewed artifact that
updates with a `git pull` rather than a per-clone file that drifts between
machines. It **fails closed**: if the push range cannot be diffed, it gates
everything rather than reporting "no changes."

`.github/workflows/ci.yml` then confirms the same thing on a clean checkout and
gates outside contributions. It is path-filtered behind a `changes` job,
cancels superseded runs on main as well as PRs, restricts cache *writes* to
main (many concurrent writers against GitHub's 10 GB budget were LRU-evicting
each other — the mechanism behind the empty cache), keys the llama.cpp CMake
tree on `llama-cpp-sys-4`'s version rather than the `Cargo.lock` hash
(`Cargo.lock` churned 29× in July; that version has changed 4× ever), and
invokes `scripts/sovereign-test.sh` so CI and local share one definition of
"the tests pass" — which also closed a real coverage hole, since bare
`cargo test --workspace` had never exercised the `sovereign-cli/dev-tools`
suites. Merge-blocking status is aggregated into a single `CI OK` job, which
treats `skipped` as success so path filtering stays compatible with branch
protection. `.github/dependabot.yml` is monthly and single-grouped: its update
runs alone had cost 19% of total spend to produce PRs that were mostly closed.

### Run

```sh
# Sovereign desktop
cd sovereign/crates/sovereign-desktop && npm install && cargo tauri dev

# Sovereign CLI — user-facing surface is `sovereign <verb>`,
# dispatching into one of four binaries. Build all four for the
# full surface, or just the dispatcher for delegator-only edits.
cargo build --release \
  -p sovereign-cli -p sovereign-cli-daemon -p sovereign-cli-dev -p sovereign-cli-llm
target/release/sovereign --help                # via the dispatcher
target/release/sovereign-cli-daemon daemon run # the long-running host

# Sovereign HTTP server
cargo build --release -p sovereign-server
target/release/sovereign-server --config sovereign/sovereign-server.toml

# cmnwlth daemon
cargo build --release -p commonwealth-daemon
target/release/commonwealth-daemon init --name "Co-op"
target/release/commonwealth-daemon daemon start
```

Default ports:

| Port  | Service                                                       |
|-------|---------------------------------------------------------------|
| 9741  | cmnwlth/Sovereign client API (OpenAI-compatible)         |
| 9742  | cmnwlth/Sovereign internal API (plaintext; network-isolation trust) |
| 9743+ | `llama-server` instances                                      |
| 50051+| `rpc-server` instances for layer shards                       |
| 8080  | Sovereign HTTP server (configurable)                          |

---

## 8. Where to look for what

| You want to                                      | Read                                                                |
|--------------------------------------------------|---------------------------------------------------------------------|
| Understand the agent runtime                     | `sovereign/crates/sovereign-core/src/runtime.rs` + `runtime/handlers/` |
| See how plans are executed                       | `sovereign-core/src/executor.rs`                                    |
| Add a tool                                       | `sovereign-contracts/src/traits.rs` (the `Tool` trait) then a new file under `sovereign-tools/src/` |
| Run a workflow (CLI or desktop)                  | CLI: `svrn workflow run` → `workflow-host::run_workflow_in_process`. Desktop: `sovereign-desktop/src-tauri/src/workflow_commands.rs` → `run_workflow_with_provider` → `src/lib/components/workflow_run/WorkflowRunView.svelte` (the "Run" nav view) |
| Ingest a folder via the Runner (substrate prize) | CLI `corpus ingest` (`corpus_cmd/ingest.rs`) runs the document-capable `notebook` shape. Desktop `lc_ingest` (`local_corpus_commands.rs`) runs it behind `SOVEREIGN_RUNNER_INGEST` (opt-in; bespoke `LocalCorpusManager::ingest` is still the default + owns OCR/enrichment). See `docs/specs/WORKFLOW_SUBSTRATE.md` roadmap |
| Add a corpus extractor                           | `corpus-engine/src/extractors/` then register in `engine/ingest.rs` |
| Add a corpus filter                              | `corpus-engine/src/filters/` (impl `DocumentFilter`) + `recipe.rs::FilterConfig` + `filters/loader.rs` |
| Bundle a generated data file in corpus-engine    | Place in `sovereign-recipes/<corpus>/data/`, append filename to `corpus-engine/build.rs::BUNDLED_ASSETS`, `include_bytes!(concat!(env!("OUT_DIR"), …))` in `filters/assets.rs` |
| Write a recipe                                   | `sovereign-recipes/<id>/recipe.toml` then add to `registry.toml` |
| Author a recipe via the agent loop               | `studio/crates/sovereign-recipe-author/` + skill at `sovereign/modes/recipe-author/skill.toml` |
| Add an `http_api` recipe (REST source)           | See `corpus-engine/src/recipe.rs` round-trip tests                  |
| Add an investigation recipe                      | `enrichment.type = "investigation"` + `[[entity_types]]` + `[[relationship_types]]` + `[[patterns]]`; run via `svrn enrich investigation build <id>` |
| Write a skill                                    | `sovereign/modes/<id>/skill.toml`                                   |
| Tune model selection per hardware                | `sovereign/models.toml`                                             |
| Understand the SCIP call graph                   | `corpus-engine-scip/` (`scip_graph.rs`, `scip_export.rs`)           |
| See the code-intelligence MCP server             | `sovereign/crates/sovereign-cli-dev/src/project_cmd/serve.rs` (`cmd_serve`); long-running variant at `sovereign-cli-daemon/src/daemon_cmd/`(`run_daemon`) |
| See the Sovereign HTTP MCP route                 | `sovereign/crates/sovereign-server/src/routes_mcp.rs`               |
| Trace a `/v1/chat/completions` end-to-end        | `commonwealth/docs/routing-field-guide.md`                          |
| Point an outside tool (Claude Code, Codex, an Ollama client, an OpenAI SDK, an editor) at the daemon | `docs/INTEROP.md` — task-oriented recipes per socket; `docs/INTEGRATION_SURFACES.md` for which surfaces are contracts |
| Understand OICP routing                          | `oicp-types/src/lib.rs` + `sovereign-mesh/src/oicp_select.rs` (shared by both sides) + `sovereign-inference/src/selector.rs` and [`docs/inference.md`](./docs/inference.md) |
| Know what a comparable project does (exo) before designing distributed inference | `docs/internal/EXO_COMPARATIVE_STUDY.md` — teardown of exo-explore/exo @ `b5375f8`: what to adopt, what we already lead on, and where both projects are stuck (notably: exo has **no** measured throughput or link-bandwidth signal either, which reframes SCHEDULER_QUALITY F10) |
| Know which CLI *use cases* are promised, and whether they still work | `docs/cli-contract.toml` — `[[command]]` rows are the verb surface, `[[journey]]` rows are the 32 **sequenced** use cases (tiered 1-5 by user impact) and `[[stranded]]` is the ledger of verbs belonging to no journey. Enforced by `cli_contract_journeys` (static ratchet), `cli_journey_dispatch` (offline), `scripts/cli-journey-verify.sh` (live read-only) and `scripts/cli-journey-sandbox.sh` (live **mutating**, boots its own daemon in a private netns on :19741). See `docs/TESTING_SURFACE.md` L4j. First live run 2026-07-28 found six real CLI defects, incl. `daemon status` writing its answer to stderr and the whole `daemon`/`project` verb family ignoring a configured `client_port` |
| Know which *promises* the CLI makes, and how much of each is actually proven | `[[experience]]` rows in the same manifest (added 2026-07-29) — 15 promises, each citing where it is promised and listing the **capabilities** it is made of; journeys declare which one they serve. `cli_contract_journeys::every_capability_is_exercised` requires each capability to be driven by a step that asserts OUTPUT (a read inline, a mutation by a later step), because every code-intelligence tool here exits **0** when it finds nothing. `MAX_UNSERVED_EXPERIENCES` is the gap register: `code-intel-chat` is declared with no journey rather than silently uncovered. `svrn contract map` renders it (or `cargo test -p sovereign-cli --test cli_contract_journeys --features dev-tools print_the_experience_map -- --nocapture` — same renderer), including the number no ratchet can fail on: steps that assert output, 77/141 repo-wide, with `correctness-loops` at 0/9 and `mesh-federation` carrying no live journey at all |
| **See what the CLI promises and how much of it can actually fail** | **`svrn contract`** (dev-tools; `map` / `census` / `nightly` views) — the one front door, added 2026-07-30 because every layer of this surface was previously reachable only by knowing it existed. `census` is the number to read: it splits the manifest into steps **a lane runs** (79: 62 assert output, 17 exit-code-only mutations proven downstream, **0 asserting nothing**) and steps **nothing runs** (62 in 14 `skip_live` journeys, 44 asserting nothing) — because a step in a never-run journey is a written intention, and adding `exit = 0` to it satisfies a ratchet without adding evidence. Four gates: `live_steps_all_assert_something`, `live_read_steps_assert_output` and `every_live_journey_asserts_output_somewhere` are **hard zeros**; `steps_no_lane_runs_do_not_grow` caps the never-run debt at 62, shrink-only. Rendered by `sovereign_cli_shared::cli_contract_report`, shared with the cargo test so the reported number and the enforced number are the same one. `svrn contract` is itself journeyed (`cli-quality` / `contract-audit`) |
| Run the *capability* half of the journey harness | Journeys declare `needs = ["operator-home" \| "indexed-repo"]` for state a throwaway sandbox cannot have. `cli-journey-sandbox.sh` passes `--lacks` for both and `cli-journey-nightly.sh` then runs exactly that remainder READ-ONLY against the operator's own daemon, so nothing is dropped by both lanes. Replaced a hardcoded `SANDBOX_EXCLUDES` array of journey ids that was invisible from the manifest |
| Understand index storage on disk                 | `corpus-engine/src/index/mod.rs`                                    |
| Understand the v2 atlas pipeline                 | [`corpus-engine/ENRICHMENT_V2.md`](../corpus-engine/ENRICHMENT_V2.md) + `corpus-engine/src/enrichment/pipeline/mod.rs` |
| Drive v2 enrichment from the CLI                 | `sovereign-cli-llm/src/enrich_cmd/`                                 |
| Understand the recipe registry                   | `corpus-engine/src/registry.rs` (+ `recipe.rs::bundled_recipe_toml`) |
| Understand delta updates                         | `corpus-engine/src/update/delta.rs`                                 |
| Understand scope expansion (filter delta)        | `corpus-engine/src/engine/expand.rs`                                |
| Understand KnowledgeView digest assembly         | `sovereign-tools/src/knowledge_view/` and [`docs/knowledge-view.md`](./docs/knowledge-view.md) |
| See where KnowledgeView is injected              | `traits.rs::LandscapeDigestProvider::splice_landscape_digests`; call sites in `runtime/streaming.rs` + `runtime/turn.rs` |
| Understand ATOS lifecycle                        | `sovereign-atos/src/local/orchestrator.rs`, `sovereign-atos/src/{charter,approval}.rs`, and [`docs/ATOS.md`](./docs/ATOS.md) |
| See the ATOS CLI surface                         | `sovereign-cli-dev/src/atos_cmd/` + `project_cmd/` (`cmd_found` in `mod.rs`, `cmd_amend` in `charter_amend.rs`, `cmd_phase` in `phase.rs`, `cmd_audit` in `audit/`) |
| Run the long-running Sovereign daemon            | `sovereign-cli-daemon/src/daemon_cmd/` + `contrib/launchd` + `contrib/systemd` |
| Rotate daemon logs                               | `sovereign-cli-daemon/src/log_rotation.rs`                          |
| Understand the loopback guard                    | `sovereign-mesh/src/loopback_guard.rs` + `admin_http::tests::loopback_guard_works_under_production_listener_shape` |
| Understand local-corpus snapshot/rollback        | `sovereign-tools/src/local_corpus/writeback.rs` + `frontmatter.rs`  |
| Pick the next daemon test to write               | [`docs/TESTING_SURFACE.md`](./docs/TESTING_SURFACE.md)              |
| Add a binary-bearing corpus (email / .docx / .xlsx / future calendar / transactions) | `corpus-engine/src/extractors/described_asset.rs` — register an `AssetSubExtractor` via `CorpusEngine::set_asset_sub_extractors`; the in-tree defaults cover xlsx / docx / plaintext / opaque |
| Read or extend the multi-origin reconciliation primitive | `corpus-engine/src/enrichment/reconciliation/{mod,multi_origin,oplog,signals}.rs` — operates on `Vec<Entity>` with `Provenance` (AD-4); writes `atlas/reconciliation_oplog.jsonl` reversible op log |
| Score a clustering of mention-ids vs ground truth (B³ + pairwise-F1) | `sovereign-eval/src/entity_resolution_score.rs` (scorer) + `entity_resolution_bench.rs` (Split/peek-budget) |
| Run the Phase 5 Enron measurement loop | `svrn bench enron run --corpus enron-sample-onemailbox --split train --policy {pre_reconciliation\|tuned}` → `sovereign-cli-llm/src/bench_cmd/enron.rs` |
| Add another typed Entity column-extractor for tabular asset kinds | `corpus-engine/src/extractors/column_aware.rs` — extend `ColumnHeaderMap` or write a per-asset-kind extractor reading the parquet parsed-form cache directly |
| Content-addressed asset store on disk | `corpus-engine/src/asset_store/{mod,fs,ledger}.rs` (AD-1; raw bytes + parsed-form caches + append-only ledger under `<corpus>/assets/`) |

| Is any quality subsystem's posture stale? | **`svrn posture`** (dev-tools) — one read-only table: artifact age + verdict for drift / arch / capability / contract-nightly / watchers / env-gate / bench baselines; each row names its refresh command. Added 2026-07-30 because drift and arch had both been weeks stale with nothing aggregating that fact |

### 8.1 Where configuration and state live

The system's configuration and mutable state live on four roots. The rule that
holds them together (center-of-mass program, 2026-07-30): **path derivations
come from the SSOT accessors** — `sovereign_contracts::rebrand`
(`svrnmesh_root` / `data_dir` / `projects_json` / `work_atlas_toml` /
`drift_dir` / `state_db_path` / `mesh_data_dir`) or their
`sovereign_cli_shared::dirs` wrappers — enforced by a `clippy.toml`
`disallowed-methods` ban on hand-rolled `dirs::home_dir` joins. **Env-var
overrides are declared** in `quality/env-flags.toml` (enforced by `cargo run
-p xtask -- env-gate`; human view generated at `docs/ENV_FLAGS.md`), and ~25
of them shadow `SetupConfig` fields — declared debt via the registry's
`shadows` key, unification deferred to `quality/CLEANUP.md`.

**Committed contracts (versioned, reviewed):**

| Surface | What it declares | Writer | Enforced / read by |
|---|---|---|---|
| `quality/ARCH_LAYERS.toml` | crate layer map + exceptions | humans | `cargo xtask layer-gate`, `arch_report` |
| `quality/env-flags.toml` | the env-knob registry (cluster/default/status/`alias_of`/`shadows`) | humans | `cargo xtask env-gate` + pin-tests in the two in-code flags tables |
| `quality/baselines/` | shrink-only ratchet baselines | **machine only** (`--update-baseline` / `--tighten`) | every count-based xtask gate |
| `docs/cli-contract.toml` | CLI verbs, journeys, experiences | humans | `cli_contract_journeys`, `svrn contract` |
| `models.toml` | model selection per hardware | humans | daemon model selection |
| `../sovereign-recipes/registry.toml` | recipe registry | humans | `corpus-engine/src/registry.rs` |
| `../clippy.toml` | lint budgets + the path-SSOT ban | humans | clippy / lint-gate ratchet |

**Repo-local `.sovereign/` (per-checkout):** `project.toml` + `project.json`
(project identity — triplicated with the per-user `projects.json`; on the
CLEANUP ledger), `sovereign.toml` (per-repo daemon/watcher posture — watchers
deliberately off in this repo), `notes.db` (gossiped working notes; dual-homed
with the per-user root via the `active_notes_db` pointer — CLEANUP),
`mesh.db` (CLI work-atlas claims — **split-brain**: the daemon keeps its
atlas store in memory, so CLI claims never gossip; CLEANUP), `features.db`
(ATOS), `SOVEREIGN.md` (the repo charter agents read).

**Per-user root** — `~/.svrnmesh`, with `~/.sovereign` as the transitional
symlink the migrator leaves behind (`rebrand::run_startup_migration`); on this
host the migration has run. Key members: `config.toml` (`SetupConfig` — THE
per-user config; note the accumulating `config.toml.bak-*` experiment
siblings), `work-atlas.toml` (atlas node config + privacy default),
`projects.json` (project registry — writer `sovereign project register` and
readers now share one accessor), `~/.svrnmesh/indexes/<corpus>/` (LanceDB
chunks, `scip_graph.db`, atlas), `~/.svrnmesh/drift/` (drift-report mirror),
`~/.svrnmesh/arch/` + `~/.svrnmesh/capabilities/` (posture artifacts),
`~/.svrnmesh/sessions/` (session-continuity frames), plus the models /
corpora / recipes / logs trees, `workspace`, `daemon.pid`,
`worker_owner_key.bin`, and the `active_notes_db` pointer.

**Platform data dir** — `~/.local/share/svrnmesh` (legacy `sovereign` name
still common on migrated hosts; resolve via `rebrand::mesh_data_dir`):
`mesh.json`, `node_id`, `join_key.secret` — the mesh identity, deliberately
platform-native so the desktop app and CLI share it.

---

## 9. Glossary

- **OICP** — Open Inference Capabilities Protocol (v0.3). Wire
  types in `oicp-types`. A model publishes one `CapabilityClaim`
  per kind-of-work it does well; schedulers score requests against
  claims with shared protocol-level + per-scheduler operational
  adjustments. See [`docs/inference.md`](./docs/inference.md).
- **CapabilityHint** — Validated tag identifying a kind of work.
  Standardized: `general`, `code`. Open vocabulary via `x:<tag>`.
- **Recipe** — A TOML file in `sovereign-recipes` describing how
  to ingest one corpus end-to-end.
- **Registry** — The recipe catalog at
  `sovereign-recipes/registry.toml` (single source of truth).
  `corpus-engine`'s `build.rs` vendors it into `OUT_DIR` as the
  compile-time bundled snapshot; can refresh from GitHub at runtime.
- **DocumentFilter** — Trait between extract and chunk that drops
  `ExtractedDoc`s by predicate. Composable via `[[filter]]`.
- **FilterPipeline / ScopeMeta** — A recipe's filter set + its
  hash. Stored in `_corpus_meta.json`; lets a corpus expand in
  place by relaxing filters and delta-ingesting.
- **Field Model (v1)** — Five-phase enrichment (skeleton → cluster
  → align → fault lines → open questions) that analyses a corpus
  holistically rather than per-chunk.
- **Domain (v1)** — Trait encoding the epistemic conventions of a
  knowledge field (philosophy, personal, conversational, …). The single
  extension point for v1; only fully-implemented domains are registered.
- **Atlas (v2)** — Typed atom graph + `Pipeline` trait + registry +
  `ExemplarBank` + `PhaseCache`. See `ENRICHMENT_V2.md`. `PhaseCache`
  stamps each phase output with the producing model (`<phase>.model.json`
  sidecar) and declines to reuse a phase written by a different model —
  a model swap forces recomputation rather than silently mixing outputs
  (OICP v0.4 §6; keyed on `chat_model`, fingerprint deferred). Built via
  the single `EnrichConfig::phase_cache()` helper so all pipeline reads
  and writes carry the same identity.
- **SCIP** — Source Code Intelligence Protocol. `scip_graph.rs`
  stores SCIP data in SQLite; `scip_export.rs` dispatches to
  language-specific analyzers.
- **CodeWatcher** — `notify`-crate filesystem watcher. Re-indexes
  modified files via `CorpusEngine::reindex_file` and marks them
  stale in the call graph (800 ms debounce).
- **Shard** — A `corpus-engine` index containing only a contiguous
  chunk-ID range. Structurally identical to a complete index.
- **Skill** — A TOML file configuring routing triggers, planner
  templates, prompt overrides, memory rules, and OICP requirements
  for a class of work.
- **Slot** — A model-loading position in `EmbeddedLlamaCpp`
  (Quick / Main / Code / Embed). See
  [`docs/inference.md`](./docs/inference.md).
- **Mesh** — A closed trust ring of cmnwlth nodes that share
  inference and knowledge. Joined via a `cwth-XXXX-XXXX-XXXX` key.
- **Peering** — A trust relationship between two distinct meshes
  that lets them exchange models or knowledge under a chosen
  `PeerTrustLevel`.
- **EmbedFn / InferenceFn** — Function types `corpus-engine`
  accepts from its caller for embedding text and (optionally)
  running an LLM during enrichment. Keeps the engine free of any
  specific runtime.
- **EmbedModelInfo** — `{ model_id, dimensions, pooling,
  normalization }`. Cross-peer interoperability contract.
- **KnowledgeView** — Three-map landscape-digest system (personal
  memories, 180-day conversation history, institutional notes)
  spliced into the system prompt before each turn. Strict
  local-scope privacy is structural, not policy.
- **ATOS** — Agent Task Orchestration System. Two-layer
  scaffolding (project charter + per-feature specs) injecting the
  relevant contract into every agent turn, detecting SHA-256
  drift, recording decisions/deviations/milestones under
  `.sovereign/`.
- **Charter** — ATOS specification document (`CHARTER.md` for the
  project, `spec.md` for a feature). Committing it is approval.
- **Drift** — ATOS term for "spec file changed since approval."
  Warns next turn; does not block. Either revert or
  `atos spec accept`.
- **Sovereign-coder pipeline** — cmnwlth middleware chain
  (`approval_gate → session_briefing → context_injector →
  tool_injector → artifact_surface`) that adapts a generic coder
  model into an ATOS-aware one.
- **Work atlas** — Cross-mesh peer awareness:
  `work_in_flight` / `declare_scope` / `release_scope` MCP tools.
  See [`docs/WORK_ATLAS.md`](./docs/WORK_ATLAS.md).

---

## 10. Architecture roadmap

Work intentionally deferred. Listed so the next engineer inherits a
todo list rather than a surprise (per
[`ARCH_PRINCIPLES.md`](./ARCH_PRINCIPLES.md) §14.3). A big file or
a documented gap without an entry is a bug; a big file or gap with
an entry is sequenced work. The ledger holds only LIVE deferrals:
when an entry completes, its chronicle moves to
[`HISTORY.md`](./HISTORY.md) (the `setup_cmd`/`daemon_cmd`/`mesh_cmd`
splits and the commonwealth-CLI placeholder resolution live there
now) and the row is dropped — or trimmed to the still-open residual.

### 10.1 Sovereign deferrals

| Item | Location | Why deferred |
|------|----------|--------------|
| `project_cmd.rs` split — **DONE 2026-07-13** | `sovereign-cli-dev/src/project_cmd/` (dispatcher `mod.rs` 645 lines, was 7,102) | Split into a directory module — `audit/`, `init/`, `serve.rs`, `refresh.rs`, `scaffold.rs`, `charter_amend.rs`, `registry_watch.rs`, `hooks.rs`, `phase.rs`, `design_plan.rs` — every file under the ARCH §3.1 1,200-line ceiling. `mod.rs` keeps `run_project` dispatch + the shared daemon/git/date plumbing; each command family is one findable file. (`sovereign-cli-dev` remains feature-gated out of the public build behind `--features dev-tools` — the rationale the `atos_cmd/run.rs` row still references.) |
| `model_slot.rs` residual (was the `embedded.rs` split) | `sovereign-inference/src/embedded/model_slot.rs` (~3,475 lines) | The residual of the `embedded.rs` decomposition ([HISTORY](./HISTORY.md#embeddedrs--embedded-pr5b--2026-06-10)): the slot state machine + decode loops + MTP — one tight, unsafe-heavy (44 blocks) FFI concern whose remaining seam is an alternate inference backend at the `InferenceProvider` boundary, not a file split. |
| `streaming.rs` refusal-retry duplication | `sovereign-core/src/runtime/streaming.rs` (~2,900 lines) | The 2026-06-10 runtime.rs decomposition moved the streaming dispatch here intact. Its KQ and Deep/Simple synthesis loops carry two NEAR-duplicate refusal-retry state machines that genuinely differ (error-frame + finish-reason handling) — unifying them is a measured behavior change, not a move. Same deferral class for the streaming-vs-non-streaming setup duplication (turn.rs). |
| `state.rs` decomposition (desktop) | `sovereign-desktop/src-tauri/src/state.rs` (~1,730 lines, was 2,347) | Contiguous phases are extracted ([HISTORY](./HISTORY.md#staters-desktop--extraction-of-the-contiguous-phases-2026-06-09)). The remaining bootstrap body — the `tools` registry and the `EmbeddedDaemon` wiring — stays inline *by necessity, not omission*: both are **interleaved** across the whole bootstrap (tools registered before AND after `corpus_engine`; `mesh.set_*` spread over four sites and order-bound to run before `try_resume`), so neither can be a pure-relocation builder without reordering a GGUF-gated startup path. Keep `AppState` fields flat (~295 call sites borrow `state.<field>`). |
| `DesktopError` burn-down (desktop) | `sovereign-desktop/src-tauri/src/error.rs` + `src/lib/errors.ts` | The structured error + frontend mirror + zero-per-caller-edit migration enabler are in place ([HISTORY](./HISTORY.md#desktoperror--first-pr--the-burn-down-enabler-2026-06-09)). **Remaining (incremental, ~140 command modules):** flip each handler's `-> Result<_, String>` → `DesktopError` (the `?`-sites auto-convert via `From<String>`; explicit `return Err` / tail `map_err` take `.into()` or a semantic `DesktopError::upstream`/`invalid_request`) + repoint its api.ts wrapper at `invokeChecked`. The `store()`/`corpus_engine()` accessors + `require_runtime!` retirement land with the first chat-path module that needs them (deferred — chat is the live, higher-traffic path). |
| `atos_cmd/run.rs` split | `sovereign-cli-dev/src/atos_cmd/run.rs` (~4700 lines) | **De-scoped from the launch-pristine §3 bar (2026-06-08):** in the feature-gated `sovereign-cli-dev` developer toolchain (see `project_cmd.rs` row), not part of the public build. ATOS runner loop — subprocess fan-out, MCP-tool brokerage, milestone advancement, reviewer loop, run-record persistence cohere as one state machine today. One-file-per-stage split when boundaries stabilise. |
| `daemon.rs` split | `sovereign-mesh/src/daemon.rs` (~3,100 lines) | `EmbeddedDaemon` is the in-process commonwealth+sovereign entry. Pure helpers (`mesh_discovery.rs`) extracted; load-bearing splits (`app_state_builder.rs` + `background_tasks.rs`) unblocked but stay deferred until `MemberRecord.client_port` lands and a real two-daemon integration test against `start_daemon` itself can be built. |
| `inference_adapter.rs` split | `sovereign-mesh/src/inference_adapter.rs` (~2100 lines) | Pure helpers (`build_self_manifest`, `synthesize_slot_claims`) extracted to `oicp_synthesis.rs`. Wire-shape translation, tool-call envelope parsing, tool-profile policy stay until the tool-call envelope migration settles. |
| `peer_inference.rs` split | `sovereign-mesh/src/peer_inference.rs` (~2280 lines) | `MeshInferenceProvider` + throughput observation + manifest caching + quarantine. `ThroughputObservedStream` extracted to `throughput_tracking.rs`. `complete_stream_with_id_and_finish` and `complete_stream_with_id` deduplication blocked on `select_route` enum extraction. |
| `auto_ingest.rs` split | `sovereign-mesh/src/auto_ingest.rs` (~1200 lines) | Auto-collaborate orchestration — `Planning → Handoff → Active → Complete` state machine. Splitting before the cloud-peer flavour settles would re-merge. |
| `sqlite/conv_tiered.rs` residual (was the `sqlite.rs` split) | `sovereign-store/src/sqlite/conv_tiered.rs` (~1,100 lines) | The 2026-07-12 split landed `sqlite.rs` (4,097 lines) as a 582-line parent + 14 per-concern modules; the largest child holds the ConvTieredReader + skeleton/RAPTOR/motif methods. Next growth splits the chunk-entity methods out. |
| `scoring.rs` residual (was the `oicp-types` lib split) | `oicp-types/src/scoring.rs` (~1,260 lines) | The residual of the 2026-07-11 quality-program R2 split (lib.rs 3,005 → 68 + 9 family modules): the §6/§7 reference-scoring implementation — 15 tuning constants, the scorer chain, `NodeObservations` — coheres as one auditable algorithm today. Next seam if it grows: node-observation/locality signals vs the scorer itself. |
| `document_asset.rs` split | `sovereign-tools/src/document_asset.rs` (~3617 lines) | DocumentAssetManager — tiered (T1/T2/T3) ingest orchestration + skeleton/RAPTOR persistence. Splits along the tier boundary once the tiered surface stops evolving. |
| `found.rs` split | `sovereign-cli-dev/src/found.rs` (~2750 lines) | `svrn project found` four-stage founding conversation. Splits one-file-per-stage when the founding flow stabilises. |
| `MemberRecord.client_port` wire field | `commonwealth-core/src/mesh.rs` + `commonwealth-discovery/src/membership.rs` + `sovereign-mesh/src/daemon.rs::peer_inference_endpoints` + `sovereign-mesh/src/auto_ingest.rs` | Local-side port plumbing landed; **peer-uniformity assumption** remains: `peer_inference_endpoints` rewrites every peer URL with this daemon's client_port, and `auto_ingest` pins port `9742`. Mixed-port mesh deployments need a `client_port` field on `MemberRecord` and a matching slot in the join handshake. Until then, operators who set a non-default `client_port` should configure every peer the same. |
| Atlas inspector Phase 2 — curation overlay | `sovereign-tools/src/atlas_view/` | Phase 1 ships read-only inspection. Phase 2 adds an `atlas/overlay.sqlite` keyed by `StableAtomKey` (content-hash) so user edits and approval state survive re-extraction. Forward-compat fields (`curation_status`, `overlay_supports`) already on every DTO. |
| Imports tab — Gemini extractor | `corpus-engine/src/extractors/` + `sovereign-recipes/conversations-gemini/` | Library → Add → Conversations ships **Anthropic + ChatGPT** (2026-06) **+ email-archive** (2026-07: mbox/maildir/.eml via the parameterized recipe, no staging copy, no auto-enrich). Gemini (Google Takeout) remains: the plumbing is source-agnostic — a new `<source>_export` extractor + recipe + `ImportSource` arm + `<ConversationImportCard>` is all it takes. ChatGPT pattern (mapping-tree walk-up, PUA marker cleaning, source-aware `import_commands.rs`) is the template. |
| Imports tab — KQ chip label for conversation corpora | `sovereign-core/src/runtime/types.rs` `KnowledgeQueryPlan` | DeepQuery path threads `display_categories`; streaming KQ + metalingual locator pass `None`. Sub-page UX polish. |

### 10.1b corpus-engine deferrals

| Item | Location | Why deferred |
|------|----------|--------------|
| `recipe.rs` split | `corpus-engine/src/recipe.rs` (~4,200 lines) | Recipe TOML schema + loader + recipe-authoring tools + parameter resolution + `bundled_recipe_toml(id: &str)` dispatch. The §2-style enumify of `bundled_recipe_toml` (RecipeId enum) is a prerequisite. |
| `notes.rs` split | `corpus-engine-notes/src/notes.rs` (~5634 lines) | NoteStore façade + persistence migrations + lifecycle + decision-log tools. **Carved out of `corpus-engine` into its own crate** (blast-radius control) — that isolation was the higher-priority move; the in-file split is still wanted. SQL schemas + migrations couple tightly. |
| `entity_extraction.rs` split | `corpus-engine/src/enrichment/entity_extraction.rs` (~2930 lines) | Phase-1b entity extraction for personal + conversational domains. Active surface (recent enrichment work); split along the per-domain extractor boundary once it settles. |
| `atlas/resolution.rs` split | `corpus-engine/src/enrichment/atlas/resolution.rs` (~5,200 lines) | Atlas URI resolution + scoring. Hottest-iteration file; splitting churn-heavy code obscures git history while the algorithm is still settling. |
| `pipeline/runner.rs` split | `corpus-engine/src/enrichment/pipeline/runner.rs` (~3100 lines) | v2 atlas orchestrator. Phase dispatch + ExemplarBank + PhaseCache + step retry all touch the same state. |
| `engine/mod.rs` split | `corpus-engine/src/engine/mod.rs` (~3000 lines) | `CorpusEngine` façade. Plausible after watcher-driven recipes settle and `ingest_driver` enumify lands. |
| `pipelines/literary_atlas.rs` split | `corpus-engine/src/enrichment/pipeline/pipelines/literary_atlas.rs` (~2900 lines) | Splits naturally along phase boundaries (extract, cluster, name, resolve, synthesize). |

### 10.1c Size-debt acceptance — 2026-07-30 red-gate sweep

The arch-gate baseline was refrozen 2026-07-30 (137 oversized files) after the
May–July arcs grew the workspace past the previous freeze. Files that were
already rows above stay governed by their own entries (`conv_tiered.rs` note:
the growth its row predicted has now happened — it crossed 1,200 at 1,310).
The rows below ledger the files that became oversized WITHOUT an entry,
grouped by the arc that grew them; each group splits when its surface
stabilises, same contract as every other row in this section.

| Item | Location | Why deferred |
|------|----------|--------------|
| Mesh measurement + allocation arc | `sovereign-cli-llm/src/mesh_cmd.rs` (4,730 — grew +3,419), `sovereign-cli-llm/src/mesh_bench.rs` (2,275) + `mesh_bench/tests.rs` (1,583), `sovereign-core/src/mesh_measurements.rs` (2,925), `sovereign-mesh/src/decision_log.rs` (1,348), `sovereign-mesh/src/mesh_http.rs` (1,623), `sovereign-mesh/src/mesh_sim/mod.rs` (2,380), `sovereign-mesh/tests/mesh_sim_scoreboard.rs` (2,750), `sovereign-mesh/src/scheduler_core.rs` (1,215), `sovereign-mesh/tests/chat_completion_e2e.rs` (1,511) | The allocation/measurement/plan surface is the hottest current iteration (RunConditions, median-run headlines, gossip-travel measurements — commits through `df88e073`). Splitting mid-arc obscures blame on an algorithm still settling; `mesh_cmd.rs` splits along its verb families (plan/measure/bench) once the measurement schema freezes. |
| 122B distributed inference | `sovereign-inference/src/embedded/rpc_warm_cache.rs` (1,606), `sovereign-mesh/src/rpc_warm_http.rs` (1,236) | RPC warm-cache + its HTTP surface; ggml-RPC-over-iroh is still an open workstream (`docs/QWEN122B_DISTRIBUTED_HANDOFF.md`) — the seam moves with it. |
| Session continuity + context-spend arc | `sovereign-cli/src/session_cmd.rs` (3,215), `sovereign-tools/src/code/session_state.rs` (1,421), `sovereign-cli/src/cache_audit_cmd.rs` (2,312) | Frame lineage/objective machinery and the cache auditor grew together with `docs/specs/SESSION_CONTINUITY.md` §2; `session_cmd.rs` splits per verb (frames/attach/distill) once the frame contract stops moving. |
| CLI-contract machinery | `sovereign-cli-shared/src/cli_contract.rs` (1,390), `sovereign-cli/src/main.rs` (1,202) | Contract model + dispatcher; `main.rs` sits 2 lines over the ceiling and shrinks when the next verb family moves out-of-process. |
| Compute-child process boundary | `sovereign-compute/src/manager.rs` (1,585), `sovereign-compute/src/supervisor.rs` (1,532) | New crate (P1, 2026-07); manager/supervisor cohere as one lifecycle state machine until a second child type exists to force the seam. |
| Retrieval redesign 2026H2 | `sovereign-core/src/runtime/retrieval/history.rs` (1,310), `sovereign-core/src/runtime/retrieval/query_expansion.rs` (1,578), `sovereign-tools/src/raptor_atlas.rs` (1,389) | ANN-refine + expansion pipeline under active redesign (S4 gate validated, blocked on chunk-eviction fix); splitting before S4 lands re-merges. |
| Router calibration | `sovereign-core/src/router_calibration.rs` (1,317) | Calibration bank + heuristic floor; splits from the router once the framework-routing matrix (v1, 4 cells) grows its next cell. |
| Desktop command families | `sovereign-desktop/src-tauri/src/governance_commands.rs` (1,215), `sovereign-desktop/src-tauri/src/local_corpus_commands.rs` (1,295) | Per-surface Tauri command modules; each splits along its sub-page boundary when the Svelte-side W1–W6 work resumes. |
| Meshapp platform SDK | `sovereign-meshapp/src/wrapped.rs` (1,210), `sovereign-meshapp/src/wrapped/semantic.rs` (1,403) | Wrapped-projection + semantic layer; the `_sdk/` catalog contract is newer than either file — split follows the SDK's shape. |
| OICP studio extraction | `oicp-client/src/lib.rs` (1,754) | The corpus-engine-free client carve-out landed as one file by design (boundary-gate leaf); splits into transport/session/structured-output modules on next growth. |
| Chaos scoring | `sovereign-eval/src/chaos_monkey/score.rs` (1,381) | Rubric scorer for the reliability-report sweep; per-dimension split lands with the pinned-critic work. |
| Reindexer liveness | `sovereign-mesh/src/reindexer.rs` (1,519) | Freshness gate + heartbeat + supervisor self-heal grew together (watcher-liveness hardening); split waits for the watcher re-enable decision. |
| corpus-engine index search | `corpus-engine/src/index/search.rs` (1,213) | 13 lines over the ceiling; trims naturally when the ANN-refine retrieval fix migrates the legacy search path. |
| Iroh transport (cmnwlth) | `commonwealth/crates/commonwealth-transport/src/iroh.rs` (1,280) | No-VPN mesh arc (invite/join/gossip over iroh) landed as one transport module; splits dialer/acceptor/relay once the relay-floor characterization settles. |

### 10.2 cmnwlth deferrals

| Item | Location | Why deferred |
|------|----------|--------------|
| `frontdoor.rs` split | `commonwealth-api/src/frontdoor.rs` (~5758 lines) | Harness-protocol → model-native normalizer — 9 concerns (harness detect, tool keeplist, heredoc diagnostics, distiller, path repair, nudges, allowlists, brief). Shares path-canon / tool-rewrite logic with `routes_responses.rs`; sequenced as the harness-unification PR (extract a shared reshaping core), not a bare size split. |
| `routes_responses.rs` split | `commonwealth-api/src/routes_responses.rs` (~3140 lines) | `/v1/responses` OpenAI-adapter — request/SSE translation + tool rewriting + path canon. The path-canon + tool-rewrite halves dedupe with `frontdoor.rs` into the shared reshaping core (same PR). |
| Multi-embed-model dispatch | `commonwealth-api/src/routes_inference.rs` | `/v1/embeddings` ignores the `model` field; gated on a second production embed model. |
| `embed_batch` | `commonwealth-api/src/routes_inference.rs` | Inputs fan out one at a time; gated on a backend that batches more efficiently. |
| Knowledge replica fanout | `commonwealth-api/src/routes_knowledge.rs` | Knowledge fan-out only hits non-hosted corpora today; gated on merge-dedupe hardening. |
| mesh_store gossip replication | `commonwealth-api/src/routes_internal/` | Gossip replicates the `Mesh` member list only. The `POST /internal/app/state` receiver exists (`routes_app_internal::recv_app_state`) and explicit peer push at queue-handoff time uses it; the periodic gossip *sender* is still missing (`all_entries_for_gossip` remains test-only). |
| Mesh Health attach-mode HTTP | `commonwealth-api/src/state.rs` + `sovereign-desktop/src-tauri/src/mesh_commands.rs` | Local-mode UI works; `mesh_get_contributions` now fetches `GET /internal/contribution/view` in attach mode. Remaining: `mesh_set_peer_preference` returns an explicit "not exposed over the daemon HTTP API in Attach mode" error — the set/clear route is still missing. |
| ATOS middleware no-op fall-through | `commonwealth-api/src/routes_inference.rs` | When no session store is configured, the ATOS pipeline degrades to legacy routing. By design; operators should expect the silent fall-through. |

### 10.3 Doc posture

The two long-form commonwealth docs —
`commonwealth/ARCHITECTURE.md` and
`commonwealth/IMPLEMENTATION_PLAN.md` — are flagged at their top
as historical record. They preserve the original design rationale
(and the constitutional Design Philosophy section in
ARCHITECTURE.md still governs the project) but are not maintained
against current code shape. This file (§5 in particular) is the
source of truth for the running system. Completed-work chronicles
extracted from this file live in [`HISTORY.md`](./HISTORY.md) —
the overview states what IS, HISTORY preserves how it came to be,
and dated entries there are never rewritten to match later code.
