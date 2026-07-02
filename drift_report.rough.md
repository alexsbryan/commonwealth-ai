# Rough Edges — `drift-target`

*480 findings (10 markers · 0 doc-drift · 470 smells · 0 critical · 34 likely · 446 note)*

Source: `.`

## TODO (intent) (10)

- `commonwealth/crates/commonwealth-api/src/routes_app_internal.rs:105` — replace with proper base64 decode once dep is added.
- `commonwealth/crates/commonwealth-daemon/src/main.rs:967` — re-probe on config file change; for now, daemon restart is required.
- `corpus-engine/src/enrichment/field_engine.rs:762` — Query tables for actual counts.
- `corpus-engine/src/extractors/chatgpt_export.rs:52` — *
- `corpus-engine/src/index/enrichment.rs:439` — Implement via LanceDB merge or update API.
- `corpus-engine/src/index/enrichment.rs:452` — Implement via LanceDB merge or update API.
- `corpus-engine-archaeology/src/rough_edges.rs:651` — handle the empty case\n    // FIXME(alex): off-by-one\n}\n",
- `sovereign/crates/sovereign-inference/src/embedded/model_slot.rs:2070` — generate_stream_sync still has an end-of-fn unconditional
- `sovereign/crates/sovereign-server/src/main.rs:528` — pass reporter to WatcherCoordinator when watcher support is added
- `sovereign-mobile/src-tauri/src/connectivity/reachability.rs:20` — pin-time): per-platform interface enumeration / Tailscale

## Smells (470)

_Code smells the structural-correctness layer flags: absolute developer paths in source (portability), large files with zero tracing events (§9.1 glassbox)._

- `commonwealth/crates/commonwealth-api/src/frontdoor.rs:3742` (absolute-user-path) — absolute developer-home path in source (breaks portability)
- `commonwealth/crates/commonwealth-api/src/frontdoor.rs:4857` (absolute-user-path) — absolute developer-home path in source (breaks portability)
- `commonwealth/crates/commonwealth-api/src/frontdoor.rs:4985` (absolute-user-path) — absolute developer-home path in source (breaks portability)
- `commonwealth/crates/commonwealth-api/src/frontdoor.rs:4986` (absolute-user-path) — absolute developer-home path in source (breaks portability)
- `commonwealth/crates/commonwealth-api/src/routes_responses.rs:1591` (absolute-user-path) — absolute developer-home path in source (breaks portability)
- `commonwealth/crates/commonwealth-api/src/routes_responses.rs:2426` (absolute-user-path) — absolute developer-home path in source (breaks portability)
- `commonwealth/crates/commonwealth-api/src/routes_responses.rs:2429` (absolute-user-path) — absolute developer-home path in source (breaks portability)
- `corpus-engine/xtask/src/main.rs:361` (absolute-user-path) — absolute developer-home path in source (breaks portability)
- `corpus-engine/xtask/src/main.rs:362` (absolute-user-path) — absolute developer-home path in source (breaks portability)
- `corpus-engine-archaeology/src/rough_edges.rs:60` (absolute-user-path) — absolute developer-home path in source (breaks portability)
- `corpus-engine-archaeology/src/rough_edges.rs:390` (absolute-user-path) — absolute developer-home path in source (breaks portability)
- `corpus-engine-archaeology/src/rough_edges.rs:391` (absolute-user-path) — absolute developer-home path in source (breaks portability)
- `sovereign/crates/sovereign-cli/src/awareness_cmd/store_open.rs:121` (absolute-user-path) — absolute developer-home path in source (breaks portability)
- `sovereign/crates/sovereign-cli/src/awareness_cmd/store_open.rs:124` (absolute-user-path) — absolute developer-home path in source (breaks portability)
- `sovereign/crates/sovereign-cli-dev/src/honesty.rs:562` (absolute-user-path) — absolute developer-home path in source (breaks portability)
- `sovereign/crates/sovereign-cli-dev/src/honesty.rs:563` (absolute-user-path) — absolute developer-home path in source (breaks portability)
- `sovereign/crates/sovereign-cli-llm/src/bench_cmd/obsidian.rs:20` (absolute-user-path) — absolute developer-home path in source (breaks portability)
- `sovereign/crates/sovereign-cli-shared/src/prompts.rs:145` (absolute-user-path) — absolute developer-home path in source (breaks portability)
- `sovereign/crates/sovereign-cli-shared/src/prompts.rs:152` (absolute-user-path) — absolute developer-home path in source (breaks portability)
- `sovereign/crates/sovereign-cli-shared/src/prompts.rs:153` (absolute-user-path) — absolute developer-home path in source (breaks portability)
- `sovereign/crates/sovereign-cli-shared/src/prompts.rs:161` (absolute-user-path) — absolute developer-home path in source (breaks portability)
- `sovereign/crates/sovereign-cli-shared/src/prompts.rs:178` (absolute-user-path) — absolute developer-home path in source (breaks portability)
- `sovereign/crates/sovereign-cli-shared/src/prompts.rs:179` (absolute-user-path) — absolute developer-home path in source (breaks portability)
- `sovereign/crates/sovereign-core/src/mcp_config.rs:101` (absolute-user-path) — absolute developer-home path in source (breaks portability)
- `sovereign/crates/sovereign-core/src/mobile_host.rs:390` (absolute-user-path) — absolute developer-home path in source (breaks portability)
- `sovereign/crates/sovereign-desktop/src-tauri/src/commands/chat.rs:485` (absolute-user-path) — absolute developer-home path in source (breaks portability)
- `sovereign/crates/sovereign-desktop/src-tauri/src/commands/chat.rs:489` (absolute-user-path) — absolute developer-home path in source (breaks portability)
- `sovereign/crates/sovereign-desktop/src-tauri/src/crash_bundle.rs:251` (absolute-user-path) — absolute developer-home path in source (breaks portability)
- `sovereign/crates/sovereign-desktop/src-tauri/src/crash_bundle.rs:252` (absolute-user-path) — absolute developer-home path in source (breaks portability)
- `sovereign/crates/sovereign-desktop/src-tauri/src/crash_bundle.rs:253` (absolute-user-path) — absolute developer-home path in source (breaks portability)
- `sovereign/crates/sovereign-desktop/src-tauri/src/crash_bundle.rs:288` (absolute-user-path) — absolute developer-home path in source (breaks portability)
- `sovereign/crates/sovereign-eval/src/cognitive/scorer.rs:668` (absolute-user-path) — absolute developer-home path in source (breaks portability)
- `sovereign/crates/sovereign-tools/src/code/atos_plan_emit.rs:137` (absolute-user-path) — absolute developer-home path in source (breaks portability)
- `sovereign/crates/sovereign-tools/src/code/atos_verify.rs:104` (absolute-user-path) — absolute developer-home path in source (breaks portability)
- `commonwealth/crates/commonwealth-api/src/middleware/artifact_surface.rs:1` (zero-tracing) — 318-line file has fn/impl declarations but zero tracing::* calls (§9.1 glassbox)
- `commonwealth/crates/commonwealth-api/src/middleware/session_briefing.rs:1` (zero-tracing) — 404-line file has fn/impl declarations but zero tracing::* calls (§9.1 glassbox)
- `commonwealth/crates/commonwealth-api/src/openai_types.rs:1` (zero-tracing) — 809-line file has fn/impl declarations but zero tracing::* calls (§9.1 glassbox)
- `commonwealth/crates/commonwealth-api/src/responses_types.rs:1` (zero-tracing) — 421-line file has fn/impl declarations but zero tracing::* calls (§9.1 glassbox)
- `commonwealth/crates/commonwealth-api/src/routes_ollama.rs:1` (zero-tracing) — 650-line file has fn/impl declarations but zero tracing::* calls (§9.1 glassbox)
- `commonwealth/crates/commonwealth-api/src/routes_status.rs:1` (zero-tracing) — 330-line file has fn/impl declarations but zero tracing::* calls (§9.1 glassbox)
- `commonwealth/crates/commonwealth-core/src/activity.rs:1` (zero-tracing) — 470-line file has fn/impl declarations but zero tracing::* calls (§9.1 glassbox)
- `commonwealth/crates/commonwealth-core/src/capabilities.rs:1` (zero-tracing) — 450-line file has fn/impl declarations but zero tracing::* calls (§9.1 glassbox)
- `commonwealth/crates/commonwealth-core/src/contributions.rs:1` (zero-tracing) — 590-line file has fn/impl declarations but zero tracing::* calls (§9.1 glassbox)
- `commonwealth/crates/commonwealth-core/src/fair_sched.rs:1` (zero-tracing) — 653-line file has fn/impl declarations but zero tracing::* calls (§9.1 glassbox)
- `commonwealth/crates/commonwealth-core/src/knowledge.rs:1` (zero-tracing) — 501-line file has fn/impl declarations but zero tracing::* calls (§9.1 glassbox)
- `commonwealth/crates/commonwealth-core/src/mesh.rs:1` (zero-tracing) — 989-line file has fn/impl declarations but zero tracing::* calls (§9.1 glassbox)
- `commonwealth/crates/commonwealth-core/src/model_aliases.rs:1` (zero-tracing) — 310-line file has fn/impl declarations but zero tracing::* calls (§9.1 glassbox)
- `commonwealth/crates/commonwealth-discovery/src/gossip.rs:1` (zero-tracing) — 484-line file has fn/impl declarations but zero tracing::* calls (§9.1 glassbox)
- `commonwealth/crates/commonwealth-discovery/src/membership.rs:1` (zero-tracing) — 500-line file has fn/impl declarations but zero tracing::* calls (§9.1 glassbox)
- `commonwealth/crates/commonwealth-inference/src/scheduler/knowledge_assignment.rs:1` (zero-tracing) — 767-line file has fn/impl declarations but zero tracing::* calls (§9.1 glassbox)
- *…and 420 more (see JSON sidecar)*

