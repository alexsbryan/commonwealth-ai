# Rough Edges — `commonwealth-ai`

*272 findings (8 markers · 0 doc-drift · 264 smells · 0 critical · 17 likely · 255 note)*

Source: `/Users/alexsbryan/dev/commonwealth-ai`

## TODO (intent) (8)

- `commonwealth/crates/commonwealth-api/src/routes_app_internal.rs:86` — replace with proper base64 decode once dep is added.
- `commonwealth/crates/commonwealth-daemon/src/main.rs:952` — re-probe on config file change; for now, daemon restart is required.
- `corpus-engine/src/enrichment/field_engine.rs:693` — Query tables for actual counts.
- `corpus-engine/src/index/enrichment.rs:396` — Implement via LanceDB merge or update API.
- `corpus-engine/src/index/enrichment.rs:409` — Implement via LanceDB merge or update API.
- `corpus-engine/src/rough_edges.rs:641` — handle the empty case\n    // FIXME(alex): off-by-one\n}\n",
- `sovereign/crates/sovereign-cli/src/tools_cmd/registry.rs:34` — post-phase-2): extract the per-tool registration calls
- `sovereign/crates/sovereign-server/src/main.rs:377` — pass reporter to WatcherCoordinator when watcher support is added

## Smells (264)

_Code smells the structural-correctness layer flags: absolute developer paths in source (portability), large files with zero tracing events (§9.1 glassbox)._

- `corpus-engine/src/rough_edges.rs:56` (absolute-user-path) — absolute developer-home path in source (breaks portability)
- `corpus-engine/src/rough_edges.rs:380` (absolute-user-path) — absolute developer-home path in source (breaks portability)
- `corpus-engine/src/rough_edges.rs:381` (absolute-user-path) — absolute developer-home path in source (breaks portability)
- `sovereign/crates/sovereign-cli/src/awareness_cmd/store_open.rs:119` (absolute-user-path) — absolute developer-home path in source (breaks portability)
- `sovereign/crates/sovereign-cli/src/awareness_cmd/store_open.rs:122` (absolute-user-path) — absolute developer-home path in source (breaks portability)
- `sovereign/crates/sovereign-cli/src/eval_cmd/runner.rs:394` (absolute-user-path) — absolute developer-home path in source (breaks portability)
- `sovereign/crates/sovereign-cli/src/honesty.rs:556` (absolute-user-path) — absolute developer-home path in source (breaks portability)
- `sovereign/crates/sovereign-cli/src/honesty.rs:557` (absolute-user-path) — absolute developer-home path in source (breaks portability)
- `sovereign/crates/sovereign-cli/src/util/prompts.rs:130` (absolute-user-path) — absolute developer-home path in source (breaks portability)
- `sovereign/crates/sovereign-cli/src/util/prompts.rs:137` (absolute-user-path) — absolute developer-home path in source (breaks portability)
- `sovereign/crates/sovereign-cli/src/util/prompts.rs:138` (absolute-user-path) — absolute developer-home path in source (breaks portability)
- `sovereign/crates/sovereign-cli/src/util/prompts.rs:144` (absolute-user-path) — absolute developer-home path in source (breaks portability)
- `sovereign/crates/sovereign-cli/src/util/prompts.rs:160` (absolute-user-path) — absolute developer-home path in source (breaks portability)
- `sovereign/crates/sovereign-cli/src/util/prompts.rs:161` (absolute-user-path) — absolute developer-home path in source (breaks portability)
- `sovereign/crates/sovereign-tools/src/code/atos_plan_emit.rs:136` (absolute-user-path) — absolute developer-home path in source (breaks portability)
- `sovereign/crates/sovereign-tools/src/code/atos_verify.rs:103` (absolute-user-path) — absolute developer-home path in source (breaks portability)
- `sovereign/crates/sovereign-tools/src/mcp/config.rs:81` (absolute-user-path) — absolute developer-home path in source (breaks portability)
- `commonwealth/crates/commonwealth-api/src/middleware/artifact_surface.rs:1` (zero-tracing) — 322-line file has fn/impl declarations but zero tracing::* calls (§9.1 glassbox)
- `commonwealth/crates/commonwealth-api/src/middleware/session_briefing.rs:1` (zero-tracing) — 394-line file has fn/impl declarations but zero tracing::* calls (§9.1 glassbox)
- `commonwealth/crates/commonwealth-api/src/openai_types.rs:1` (zero-tracing) — 625-line file has fn/impl declarations but zero tracing::* calls (§9.1 glassbox)
- `commonwealth/crates/commonwealth-core/src/capabilities.rs:1` (zero-tracing) — 325-line file has fn/impl declarations but zero tracing::* calls (§9.1 glassbox)
- `commonwealth/crates/commonwealth-core/src/contributions.rs:1` (zero-tracing) — 607-line file has fn/impl declarations but zero tracing::* calls (§9.1 glassbox)
- `commonwealth/crates/commonwealth-core/src/knowledge.rs:1` (zero-tracing) — 497-line file has fn/impl declarations but zero tracing::* calls (§9.1 glassbox)
- `commonwealth/crates/commonwealth-core/src/mesh.rs:1` (zero-tracing) — 357-line file has fn/impl declarations but zero tracing::* calls (§9.1 glassbox)
- `commonwealth/crates/commonwealth-core/src/model_aliases.rs:1` (zero-tracing) — 328-line file has fn/impl declarations but zero tracing::* calls (§9.1 glassbox)
- `commonwealth/crates/commonwealth-discovery/src/gossip.rs:1` (zero-tracing) — 481-line file has fn/impl declarations but zero tracing::* calls (§9.1 glassbox)
- `commonwealth/crates/commonwealth-discovery/src/membership.rs:1` (zero-tracing) — 416-line file has fn/impl declarations but zero tracing::* calls (§9.1 glassbox)
- `commonwealth/crates/commonwealth-inference/src/scheduler/knowledge_assignment.rs:1` (zero-tracing) — 1075-line file has fn/impl declarations but zero tracing::* calls (§9.1 glassbox)
- `commonwealth/crates/commonwealth-state/src/store.rs:1` (zero-tracing) — 372-line file has fn/impl declarations but zero tracing::* calls (§9.1 glassbox)
- `commonwealth/crates/commonwealth-test-harness/src/simulated_mesh.rs:1` (zero-tracing) — 329-line file has fn/impl declarations but zero tracing::* calls (§9.1 glassbox)
- `corpus-engine/src/acquirers/http_api/pagination.rs:1` (zero-tracing) — 393-line file has fn/impl declarations but zero tracing::* calls (§9.1 glassbox)
- `corpus-engine/src/atlas_traversal/brief.rs:1` (zero-tracing) — 572-line file has fn/impl declarations but zero tracing::* calls (§9.1 glassbox)
- `corpus-engine/src/atlas_traversal/classifier.rs:1` (zero-tracing) — 556-line file has fn/impl declarations but zero tracing::* calls (§9.1 glassbox)
- `corpus-engine/src/atlas_traversal/engine.rs:1` (zero-tracing) — 666-line file has fn/impl declarations but zero tracing::* calls (§9.1 glassbox)
- `corpus-engine/src/atlas_traversal/spans.rs:1` (zero-tracing) — 405-line file has fn/impl declarations but zero tracing::* calls (§9.1 glassbox)
- `corpus-engine/src/canonical_sync.rs:1` (zero-tracing) — 313-line file has fn/impl declarations but zero tracing::* calls (§9.1 glassbox)
- `corpus-engine/src/chunkers/paragraph.rs:1` (zero-tracing) — 422-line file has fn/impl declarations but zero tracing::* calls (§9.1 glassbox)
- `corpus-engine/src/chunkers/portal_event_bullet.rs:1` (zero-tracing) — 350-line file has fn/impl declarations but zero tracing::* calls (§9.1 glassbox)
- `corpus-engine/src/design_signals.rs:1` (zero-tracing) — 848-line file has fn/impl declarations but zero tracing::* calls (§9.1 glassbox)
- `corpus-engine/src/engine/article_stats.rs:1` (zero-tracing) — 396-line file has fn/impl declarations but zero tracing::* calls (§9.1 glassbox)
- `corpus-engine/src/enrichment/atlas/analysis/gaps.rs:1` (zero-tracing) — 450-line file has fn/impl declarations but zero tracing::* calls (§9.1 glassbox)
- `corpus-engine/src/enrichment/atlas/analysis/holistic_classifier.rs:1` (zero-tracing) — 307-line file has fn/impl declarations but zero tracing::* calls (§9.1 glassbox)
- `corpus-engine/src/enrichment/atlas/analysis/tension_classifier.rs:1` (zero-tracing) — 518-line file has fn/impl declarations but zero tracing::* calls (§9.1 glassbox)
- `corpus-engine/src/enrichment/atlas/analysis/tensions.rs:1` (zero-tracing) — 1178-line file has fn/impl declarations but zero tracing::* calls (§9.1 glassbox)
- `corpus-engine/src/enrichment/atlas/atoms.rs:1` (zero-tracing) — 745-line file has fn/impl declarations but zero tracing::* calls (§9.1 glassbox)
- `corpus-engine/src/enrichment/atlas/cross_corpus.rs:1` (zero-tracing) — 750-line file has fn/impl declarations but zero tracing::* calls (§9.1 glassbox)
- `corpus-engine/src/enrichment/atlas/schema_validation.rs:1` (zero-tracing) — 1434-line file has fn/impl declarations but zero tracing::* calls (§9.1 glassbox)
- `corpus-engine/src/enrichment/atlas/writer.rs:1` (zero-tracing) — 467-line file has fn/impl declarations but zero tracing::* calls (§9.1 glassbox)
- `corpus-engine/src/enrichment/domain.rs:1` (zero-tracing) — 585-line file has fn/impl declarations but zero tracing::* calls (§9.1 glassbox)
- `corpus-engine/src/enrichment/domains/conversational.rs:1` (zero-tracing) — 496-line file has fn/impl declarations but zero tracing::* calls (§9.1 glassbox)
- *…and 214 more (see JSON sidecar)*

