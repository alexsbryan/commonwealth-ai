# Sovereign docs

Index of everything under `sovereign/docs/`. The top-of-tree docs ([`README`](../README.md), [`SYSTEM_OVERVIEW`](../SYSTEM_OVERVIEW.md), [`ARCHITECTURE`](../ARCHITECTURE.md), [`ARCH_PRINCIPLES`](../ARCH_PRINCIPLES.md)) are the system-wide map — start there. The files in this directory go deeper on one feature, one workflow, or one runbook each.

## Quickstart & operator basics

Three short docs people hit first.

- [`FAQ.md`](FAQ.md) — common questions about offline mode, models, ports, mesh
- [`TROUBLESHOOTING.md`](TROUBLESHOOTING.md) — symptom → diagnosis pairs; run `sovereign doctor` first
- [`DEVELOPMENT.md`](DEVELOPMENT.md) — building from source, crate layout, adding tools / corpora / skills

## Reference

The "look it up" docs — flag tables, tool inventories, schema specs.

- [`CLI_REFERENCE.md`](CLI_REFERENCE.md) — every `sovereign-cli` subcommand, flags, deprecations
- [`CORRECTNESS_TOOLING.md`](CORRECTNESS_TOOLING.md) — `eval`, `voice eval`, `enrich atlas-eval`, `reading-diag` — pick the right tool
- [`specs/`](specs/README.md) — in-flight design proposals, reference patterns, and canonical wire specs (incl. OICP v0.1.0 protocol)

## Feature deep-dives

Stable behaviours users interact with beyond the quick-start path.

- [`FEATURES.md`](FEATURES.md) — what's wired beyond setup / project init / mesh create
- [`KNOWLEDGE_BASES.md`](KNOWLEDGE_BASES.md) — curated corpora (Wikipedia / SEP / OpenAlex / Stack Exchange / Gutenberg / CRS)
- [`knowledge-view.md`](knowledge-view.md) — KnowledgeView: personal / conversational / institutional memory maps
- [`PLAN_ALIGNMENT.md`](PLAN_ALIGNMENT.md) — the four alignment questions every `~/.claude/plans/` plan answers
- [`DRIFT_DETECTION.md`](DRIFT_DETECTION.md) — `sovereign drift detect`: narrative-vs-code drift
- [`GIT_ARCHAEOLOGY.md`](GIT_ARCHAEOLOGY.md) — `sovereign git-archaeology`: provenance + co-evolution per atom
- [`ARCHAEOLOGY_EVAL.md`](ARCHAEOLOGY_EVAL.md) — `sovereign archaeology-eval`: witness checks + baseline diff + inquiries

## ATOS — Agent Task Orchestration

The charter / phases / runner stack lives across four docs. Start with `ATOS.md`.

- [`ATOS.md`](ATOS.md) — the full system: design → plan → charter → phases → milestones
- [`ATOS_RUNNER.md`](ATOS_RUNNER.md) — the ralph-wiggum loop: spawn driver, judge against charter, repeat
- [`ATOS_RUNNER_SMOKE.md`](ATOS_RUNNER_SMOKE.md) — smoke-test runbook for the runner against `oicp-types`

## Runbooks

End-to-end operator workflows + deployment patterns.

- [`TOOLBOX_SETUP.md`](TOOLBOX_SETUP.md) — AMD Strix Halo via toolbox containers (Fedora/Ubuntu, ROCm/Vulkan)
- [`CLOUD_PEER_DEPLOY.md`](CLOUD_PEER_DEPLOY.md) — spin up a transient cloud GPU as a sovereign-mesh worker
- [`BENCHMARKING.md`](BENCHMARKING.md) — embed-decode throughput across Metal / Vulkan / ROCm

Prospect-facing demo walkthroughs (different audience, dated, not
maintained) live under [`../handoff/`](../handoff/README.md).

## Experiments

Historical experiment writeups have been moved to
[`archive/`](archive/README.md); the durable lessons live in the
NoteStore (`sovereign notes --query <topic>`). The reranker code
stays in-tree but is opt-in via env vars; the writeup is at
[`archive/RERANK_EXPERIMENT.md`](archive/RERANK_EXPERIMENT.md).

## Internal / contributor

Implementation conventions live next to the crate that owns them as
`AGENTS.md` (mirroring root `AGENTS.md`). Pair with
[`../ARCH_PRINCIPLES.md`](../ARCH_PRINCIPLES.md).

- [`../crates/sovereign-desktop/AGENTS.md`](../crates/sovereign-desktop/AGENTS.md) — Svelte 5 + runes + XState state-management tiers, immutability rule, Tauri-event discipline

## Examples

Templates and reference artifacts.

- [`examples/plan_v0_brief_aligned.md`](examples/plan_v0_brief_aligned.md) — example plan that satisfies the four alignment questions

## Archive

Historical status docs — kept for context, not current. See [`archive/README.md`](archive/README.md).
