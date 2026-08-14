# Workflow substrate — reuse-or-reject note

Steer directive b3a9213a (operator-approved unedited), order
`deep-research-t1a`. Question: evaluate the house workflow substrate —
`sovereign-workflow` (Workflow/StepRegistry/Runner, `FileArtifactCache`,
`StepObserver`) and `sovereign-workflow-host`
(`run_workflow_with_provider`, standard_registry, corpus installer) — as
the HOST for:

- (a) the run surface,
- (b) artifact storage + checkpointing — the ICDs AS workflow Artifacts
  (FR-2 in shipped form),
- (c) the stage observer (the DEMO-1 strip),
- (d) the gym deck MockBackend steps (T1b).

The directive already rules the loop control flow OUT of the substrate
("R11's state machine + budget decider + egress stay bespoke and thin; a
TOML DAG cannot express the terminal semantics or the FR-4 Go/No-Go
polls") — this note evaluates the four candidate surfaces, records the
layering check, and answers the demo-surface question. **Reject is a
legal outcome only with the reasons recorded** (ARCH §10.6/§19) — this
file is that record.

## The substrate, inventoried (what was actually surveyed)

| piece | where | shape |
|---|---|---|
| `Workflow`/`StepRegistry` | `studio/crates/sovereign-workflow/src/model.rs`, `runner.rs` | TOML DAG of typed steps, per-item `for_each` semantics |
| `Runner::run` | `sovereign-workflow/src/runner.rs:102` | `run(&self, wf, concurrency) -> Result<RunReport>`; `RunReport { workflow, items: Vec<ItemReport> }` with `ok_count`/`failed_count` |
| `FileArtifactCache` | `sovereign-workflow/src/cache.rs:59` | step-output cache, keyed `cache_key(uses, step_id, args, item_fingerprint)` |
| `StepObserver` / `WorkflowProgress` | `sovereign-workflow/src/progress.rs:17,33` | `Arc<dyn Fn(WorkflowProgress)>`; events `RunStarted` / `StepDone` / `ElementSkipped` / `ItemDone` / `RunFinished` — per-step, per-item, display-ready |
| `run_workflow_with_provider` | `sovereign-workflow-host/src/lib.rs:418` | the embedding entry: `(wf, inference?, installer?, concurrency, no_cache, params, extra_tools, observer?) -> Result<RunReport>`; `standard_registry` includes `corpus_store`; `HttpCorpusInstaller` |
| CLI surface | `sovereign/crates/sovereign-cli-llm/src/workflow_cmd.rs` | `svrn workflow`; `notebook` starter scaffolds a workflow whose `tool:corpus_store` step hands off a built corpus (workflow_cmd.rs:593-596) |

The notebook precedent is real and shipped: a TOML workflow already runs
`corpus_store` as a step and the CLI detects the built corpus for
handoff. That is precisely why the boundary question matters — the
substrate CAN host corpus-building steps; the question is whether it can
host the LOOP's surfaces.

## The layering check (the gate)

`quality/ARCH_LAYERS.toml` — the map is total (every workspace member
matches exactly one layer) and the parser reports `Violation::UpwardEdge`
for edges that climb (quality/arch-layers/src/lib.rs:258):

- `sovereign-core` (and `sovereign-store`) — layer **runtime**.
- `sovereign-workflow`, `sovereign-tools-base` — layer **runtime** (same
  layer: `sovereign-workflow` was placed there by the studio package's
  contract-only design).
- `sovereign-workflow-host` — layer **capabilities** (ABOVE runtime; it
  owns the tool registry + installer + embedding runner).

Consequences, both decisive:

1. A sovereign-core → `sovereign-workflow` edge is **same-layer, legal**
   — the plan errata's warning ("check ARCH_LAYERS.toml before a
   sovereign-side loop takes a studio dep") resolves to: the *base*
   workflow crate is reachable, the *host* is not.
2. A sovereign-core → `sovereign-workflow-host` edge is an **UpwardEdge
   violation** — and the substrate's two embedding surfaces the directive
   names (`run_workflow_with_provider`, the corpus installer, the
   standard tool registry) all live in the host crate. The loop cannot
   call any of them from `sovereign-core` without either a layer
   violation or a new grandfathered exception — and the errata precedent
   (the `sovereign_tools::web` reachability errata, PLAN.md §1) is
   exactly the class of mistake this check exists to prevent.

This single fact settles (a), (d), and the demo-surface question below
before any shape argument — but the shape arguments run the same
direction, so they are recorded too.

## Candidate verdicts

### (a) The run surface — REJECT

Two independent reasons, either one sufficient:

- **Layer**: the run surface a host would offer is
  `run_workflow_with_provider` (capabilities) — unreachable from
  sovereign-core without an UpwardEdge violation.
- **Shape**: even the same-layer `Runner::run` is DAG-shaped — one
  `RunReport` of per-item ok/failed, tolerant `for_each` skip semantics.
  The loop needs typed terminals (Done / DonePartial / Aborted, distinct
  verdicts), a run-scoped lock (F19), abort-from-every-state, and the
  FR-4 Go/No-Go polls between rounds. None of those is expressible in a
  TOML DAG (the directive's own statement), and re-deriving them from a
  `RunReport` would be a second implementation of the state machine —
  the exact duplication ARCH §10.6 forbids. The loop's R11 machine stays
  C-class, in `sovereign-core/src/deep_research/state.rs`, and the run
  surface is the thin `svrn deep-research` verb (hosts layer — where the
  workflow CLI also lives).

### (b) Artifact storage + checkpointing (ICDs as workflow Artifacts) — REJECT

FR-2 ("the ICDs are the checkpoints") is already shipped in its correct
shape: the run dir IS the artifact store — `charter.json`,
`budget-ledger.json`, per-round `gap_list`/`evidence_window`,
`manifest.json`, `report.md`, all file ICDs with field-level schemas,
golden fixtures pinning them (`tests/golden/`, charter hash
`e55d99dbe827fc3f`), and the budget ledger journaled *synchronously
before every spend* (fail-closed — an allowance unit is consumed by the
attempt, recorded first).

- `FileArtifactCache` is a **step cache**, keyed
  `(uses, step_id, args, item_fingerprint)` and bypassable via the
  runner's `no_cache` toggle — a durability story on a cache toggle is
  not a durability story. A fail-closed spend journal cannot ride a
  layer that may be told "no cache".
- Re-shaping the ICDs as workflow Artifacts would mean re-keying the
  run's records to step/item identity and re-fixing the schemas — the
  schemas are fixed and pinned; the note lands after that pin, so this
  is the honest sequence recorded here: the substrate was evaluated
  before the loop ships, the ICDs ship as files, and nothing in the
  substrate offers a run-scoped, fail-closed, charter-hashed record
  store that files do not already provide.

**Seam noted (not built):** the ICDs are already artifact-SHAPED (typed
files in a run dir). A future generic artifact browser could READ them
from outside the loop — reading is not hosting.

### (c) The stage observer (DEMO-1 strip) — ADOPT THE SHAPE, REJECT THE PAYLOAD

The observer *pattern* is the right pattern and gets adopted:

- `StepObserver = Option<Arc<dyn Fn(Event)>>`, injected at run start,
  `None` = headless, events go to `tracing` only
  (progress.rs:17, runner.rs:90-95) — exactly the DEMO-1 strip needs, at
  near-zero surface.
- The *payload* cannot be reused: `WorkflowProgress` is
  per-step/per-item (RunStarted/StepDone/ElementSkipped/ItemDone/
  RunFinished). The loop's stages are rounds, gap audits (verdicts),
  budget spends, and terminal transitions — translating those into
  `StepDone{item, step, uses}` rows would be lossy and would re-derive
  loop semantics from workflow vocabulary (a second naming of one
  thing). The loop's stage observer stays loop-native (typed events for
  round/gap/budget/terminal), shaped like the substrate's: injected,
  optional, tracing-only when absent.

### (d) The gym deck MockBackend steps (T1b) — NOT BUILT HERE; SEAM RECORDED

T1b is out of this order's scope (no measurement arms, no gym deck).
The seam is already half-adopted: the loop's web_search leg runs through
`sovereign-tools-base::web::search` — the registry + orchestrator whose
`WebSearchRegistry` holds `MockBackendImpl` — so a T1b gym deck can
drive the port with the substrate's own mock backend instead of a
workflow registry. Recorded as the T1b candidate; nothing built now.

## The demo-surface question

**Can DEMO-1 ride the shipped workflow run surface?** No. The shipped
workflow surface executes TOML DAGs of tool steps and returns per-item
ok/failed tallies; DEMO-1's load-bearing content is the loop's typed
terminal state, the verdict-stamped report, the budget ledger, and the
re-ask collapse — none of which a DAG run can carry or express. DEMO-1
rides the shipped CLI verb (`svrn deep-research`, the loop's own thin
surface), which is the honest answer to "reuse the run surface": the
hosts layer already has one CLI verb per surface; the workflow verb and
the deep-research verb are siblings there, not one inside the other.

## Verdict

**Reject the substrate as the loop's host, with the reasons above
recorded; adopt two shapes from it.** The layering wall (capabilities
host is unreachable from runtime) and the shape mismatches (DAG vs typed
state machine, step cache vs fail-closed journal, per-step events vs
round/gap/verdict events) each independently rule out hosting. The two
shapes adopted: the optional injected observer pattern (DEMO-1 strip)
and the registry/orchestrator-with-mock web backend (already the loop's
web_search leg). The loop control flow stays C-class, bespoke, and thin
— as the directive requires — and the ICDs stay file checkpoints, with
FR-2 satisfied in shipped form.

Written 2026-08-14, before the loop ships (this commit wave).
