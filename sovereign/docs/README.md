# Sovereign docs

Two kinds of docs live here: docs for **using Sovereign**, and docs for **working on it**. The system-wide map is one level up — [`README`](../README.md), [`SYSTEM_OVERVIEW`](../SYSTEM_OVERVIEW.md), [`ARCH_PRINCIPLES`](../ARCH_PRINCIPLES.md) — start there either way.

## Using Sovereign

- [`FAQ.md`](FAQ.md) — common questions: offline mode, models, ports, the mesh
- [`TROUBLESHOOTING.md`](TROUBLESHOOTING.md) — symptom-to-fix pairs; run `sovereign doctor` first
- [`CLI_REFERENCE.md`](CLI_REFERENCE.md) — every command and flag
- [`CODE_INTELLIGENCE.md`](CODE_INTELLIGENCE.md) — index your codebase: symbols, call graphs, search, and the tools your AI harness gets
- [`FEATURES.md`](FEATURES.md) — what the assistant does beyond the basics
- [`KNOWLEDGE_BASES.md`](KNOWLEDGE_BASES.md) — the corpora you can install, and how search uses them

Running it on particular hardware or in the cloud:

- [`TOOLBOX_SETUP.md`](TOOLBOX_SETUP.md) — AMD Strix Halo via toolbox containers (ROCm / Vulkan)
- [`CLOUD_PEER_DEPLOY.md`](CLOUD_PEER_DEPLOY.md) — add a cloud GPU as a mesh worker
- [`RUNBOOK.md`](RUNBOOK.md) — operating the inference stack day to day
- [`BENCHMARKING.md`](BENCHMARKING.md) — embed/decode throughput across Metal / Vulkan / ROCm

Building on the platform:

- [`ATOS.md`](ATOS.md) — Agent Task Orchestration: design → plan → charter → phases → milestones
- [`ATOS_RUNNER.md`](ATOS_RUNNER.md) — the runner loop, and [`ATOS_RUNNER_SMOKE.md`](ATOS_RUNNER_SMOKE.md), its smoke test
- [`MESHAPP_AUTHORING.md`](MESHAPP_AUTHORING.md) — write a mesh app; [`MESHAPP_CONSUMER.md`](MESHAPP_CONSUMER.md) — install and run one

## Working on Sovereign

Start with [`DEVELOPMENT.md`](DEVELOPMENT.md): building from source, the crate layout, the CLI binaries, and how to add a tool, corpus, or skill. The design rules are in [`../ARCH_PRINCIPLES.md`](../ARCH_PRINCIPLES.md); per-crate conventions live in each crate's `AGENTS.md`.

The rest of this folder is subsystem deep-dives and design notes, one feature or workflow each. They're linked from the code that owns them and indexed in SYSTEM_OVERVIEW's "Subsystems with their own docs" section; they're listed here so you can see what the folder holds.

Subsystems:

- [`inference.md`](inference.md) — slots, OICP scoring, harness adapters, cutoff legibility
- [`retrieval-pipeline.md`](retrieval-pipeline.md) — the retrieval steps and their knobs (generated from the code)
- [`TIERED_RETRIEVAL.md`](TIERED_RETRIEVAL.md) — the tiered retrieval surface
- [`knowledge-view.md`](knowledge-view.md) — KnowledgeView: your terrain, not your transcript
- [`notes-mesh.md`](notes-mesh.md) — how NoteStore propagates across the mesh
- [`WORK_ATLAS.md`](WORK_ATLAS.md) — coordination for agents on a shared mesh (declare / observe scope)
- [`DELEGATION_SUBSTRATE.md`](DELEGATION_SUBSTRATE.md) — the delegation substrate
- [`MESH_LOAD_AWARENESS.md`](MESH_LOAD_AWARENESS.md) — cluster-wide load awareness for mesh inference

Tooling and correctness:

- [`CORRECTNESS_TOOLING.md`](CORRECTNESS_TOOLING.md) — `eval` / `voice eval` / `reading-diag`: which tool when
- [`DRIFT_DETECTION.md`](DRIFT_DETECTION.md) — `sovereign drift detect`: narrative-vs-code drift
- [`GIT_ARCHAEOLOGY.md`](GIT_ARCHAEOLOGY.md) — provenance and co-evolution per atom
- [`ARCHAEOLOGY_EVAL.md`](ARCHAEOLOGY_EVAL.md) — witness checks, baseline diff, inquiries
- [`TESTING_SURFACE.md`](TESTING_SURFACE.md) — the daemon testing surface and priority matrix
- [`PLAN_ALIGNMENT.md`](PLAN_ALIGNMENT.md) — the four questions every plan answers

Design notes and studies — [`QUERY_TAXONOMY_MECE.md`](QUERY_TAXONOMY_MECE.md), [`RETRIEVAL_DISCRIMINATION_PLAN.md`](RETRIEVAL_DISCRIMINATION_PLAN.md), [`SOLVER_DESIGN.md`](SOLVER_DESIGN.md), [`TDD_MACHINE.md`](TDD_MACHINE.md) and [`TDD_MACHINE_DESIGN.md`](TDD_MACHINE_DESIGN.md), [`SITUATED_HARNESS_STUDY.md`](SITUATED_HARNESS_STUDY.md), [`EPHEMERAL_WORKER_PODS.md`](EPHEMERAL_WORKER_PODS.md), [`PINNED_WORKER_AS_INFERENCE_PEER.md`](PINNED_WORKER_AS_INFERENCE_PEER.md), [`OCR_PADDLE_ENGINE.md`](OCR_PADDLE_ENGINE.md), [`HANDOFF_atlas_directs_retrieval.md`](HANDOFF_atlas_directs_retrieval.md) — each captures a decision or a one-off; the durable lessons also live in the NoteStore (`sovereign notes --query <topic>`).

## Specs, examples, archive

- [`specs/`](specs/README.md) — in-flight design proposals and the canonical OICP wire spec
- [`examples/plan_v0_brief_aligned.md`](examples/plan_v0_brief_aligned.md) — a plan that satisfies the alignment questions
- [`archive/`](archive/README.md) — historical experiment writeups and status docs, kept for context

Prospect-facing demo walkthroughs (a different audience, dated, not maintained) are under [`../handoff/`](../handoff/README.md).
