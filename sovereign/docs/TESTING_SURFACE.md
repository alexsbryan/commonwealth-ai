# Daemon Testing Surface — Audit + Priority Matrix

**Last audited:** 2026-05-13 (rounds 1+2+3+4 + harness extraction + fan-out bugfix). Refresh whenever a row's coverage changes,
a capability lands, or a deferral resolves. Out-of-date rows are a bug
per ARCH §1.1 — feature docs describe *intent*, and this doc's intent
is to drive the next test.

## What this is

A living inventory of every capability the `EmbeddedDaemon`
(`sovereign-mesh::daemon`) exposes through HTTP, lifecycle methods, or
background tasks — with its current test coverage and its
user-facing impact-of-regression score. Bucketed so engineers can pick
the next gap with the highest ratio of (impact saved) / (cost to test).

This is the source-of-truth for "what test do I write next?". Reach
into it before designing a one-off; the matrix may already show that
a peer test exists or that the gap is intentionally deferred.

## Layering principle

A test belongs at the lowest layer that can catch the failure mode
it's targeting. Subsystem integration (real HTTP + real SQLite +
mocked external) is the sweet spot for daemon work — unit tests miss
wiring bugs, true E2E is slow to write and slow to run. ARCH §12 holds.

The four layers we use:

| Layer | Shape | Lives in | Catches |
|---|---|---|---|
| **L1** Unit | Pure function or single struct, no I/O | `src/*.rs` `#[cfg(test)] mod` | Math bugs, type bugs |
| **L2** Subsystem integration | Real HTTP/SQL/MeshStore on ephemeral port, mocked inference + corpus | `tests/*.rs` | Wiring bugs, route bugs, persistence-on-mutation bugs, OICP decisions |
| **L3** Multi-daemon E2E | Two+ `EmbeddedDaemon` instances on distinct ports, gossip + handshake over real HTTP | `tests/*.rs` (unblocked by the port-config landing) | Cross-mesh convergence, distributed state, peer-routing decisions |
| **L4** Real-binary smoke | `cargo run --bin sovereign daemon run` against a curated fixture | manual playbook or `--ignored` tests | mDNS, launchd/systemd, real GPU inference, real Tailscale topology |

Most production bugs in a daemon like this live at **L2 × hard
failure or restart recovery**. That's where the highest-leverage
gaps below sit.

## Impact tiers

Impact = "what breaks for the user if this regresses?".

- **P0 — Silent corruption.** Request succeeds, response looks fine,
  state is wrong. Examples: ledger underreports, persistence drops a
  member, privacy invariant slips, mutation hook silently no-ops.
- **P1 — User-facing failure.** Request fails or returns wrong content
  visibly. Examples: 503 model_not_ready when local inference is wired,
  auth boundary lets a non-loopback caller through, routing picks the
  wrong peer.
- **P2 — Degraded operation.** Functional but slower, hotter, less
  efficient. Examples: manifest cache doesn't refresh, EWMA biased,
  partition plan sub-optimal.
- **P3 — Cosmetic / observability.** Operator pain but no incorrect
  behaviour. Examples: status fields stale, log noise, stub messages.

## Coverage levels

- **✓ L2+** — Subsystem integration or higher exists.
- **~ L1** — Unit tests exist, wiring is untested.
- **·** — No test at any layer.
- **?** — Covered indirectly; sharper test would be valuable.

## Heatmap

Counts as of audit date. Top-left cell (P0 × ·) is where the next
test should come from.

|                  | **·** (no test)   | **~** (unit only) | **✓** (integration+)  |
|------------------|-------------------|-------------------|-----------------------|
| **P0** silent    | **2**             | 5                 | 20                    |
| **P1** visible   | 7                 | 6                 | 9                     |
| **P2** degraded  | 4                 | 4                 | 3                     |
| **P3** cosmetic  | 4                 | 2                 | 1                     |

Rounds 1+2+3+4 + fan-out bugfix landed 37 tests across 14 files
plus a 1-line fix to `routes_knowledge::fanout_one_peer`. Net
P0-uncovered moved 12 → 2. Files added: `embeddings_e2e` (4),
`injection_order` (3), `node_id_persistence` (2), `loopback_parity` (7),
`gossip_auth` (3),
`storage_snapshot_e2e` (2), `peer_preference_manifest` (3),
`finish_reason_streaming` (3). One audit correction in round 3:
`/v1/admin/reload` HTTP route was marked `·` but was already covered
by the `admin_http` lib tests (which use real HTTP + spawn). Mark
fixed in this refresh.

The 12 P0-uncovered cells are where regressions hurt most and review
catches least. Those are the top-priority queue at the bottom.

---

## The matrix

Each row: capability · today's coverage · impact · test pointer / next
step. Buckets follow the daemon's structural layout.

### A. Lifecycle & wiring

| Capability | Coverage | Impact | Test / Next step |
|---|---|---|---|
| `EmbeddedDaemon::new` + service injection order | ✓ | P0 | `daemon_wiring::with_local_inference_routes_chat_completions_to_adapter` |
| `with_mesh_mutation_hook` fires on real route mutation | ✓ | P0 | `daemon_wiring::with_mesh_mutation_hook_fires_on_gossip_delta`, `join_handshake::valid_join_key_admits_new_member_and_fires_hook` |
| `create_mesh` → `start_daemon` happy path | ✓ | P1 | `mesh_http::tests::create_and_status_round_trip`, `port_config::*` |
| `join_mesh` deep-link parse → `/internal/join` → adopt | ~ | P1 | `join_handshake::joiner_can_adopt_founder_mesh_after_handshake` covers wire+adopt; full `EmbeddedDaemon::join_mesh` path (auto-leave gate, mDNS discovery, swap of self_node_id) is uncovered |
| `try_resume` from disk → reconstruct mesh + start_daemon | · | **P0** | **Gap.** A daemon restart that fails to resume silently drops the user from their mesh; persistence is read but the resume → start_daemon → first-gossip path has no test |
| `leave` clears persistence + tears down | ~ | P1 | Lib tests on `persist::clear`; no daemon-level test that `leave` then `create_mesh` works without state bleed |
| `shutdown` / `stop` graceful drain | ~ | P2 | Used by `port_config::*` but no assertion on background-task teardown |
| `SetupConfig` ports flow through | ✓ | P1 | `port_config::custom_client_port_from_setup_config_flows_to_api_address` |
| Service-injection ordering — `Arc::get_mut` no-op detection | ✓ | **P0** | `injection_order::{with_local_inference_emits_error_when_arc_already_cloned, with_mesh_mutation_hook_emits_error_when_arc_already_cloned, happy_path_does_not_emit_error_when_arc_uncloned}` |
| Concurrent `create_mesh` rejected with `AlreadyRunning` | ~ | P2 | `mesh_http::tests::create_fails_when_mesh_already_exists` covers HTTP layer; no concurrency-stress test |

### B. Inference path (`/v1/*`)

| Capability | Coverage | Impact | Test / Next step |
|---|---|---|---|
| `/v1/chat/completions` local non-streaming | ✓ | P1 | `daemon_wiring`, `chat_completion_e2e` |
| `/v1/chat/completions` local streaming | ✓ | P1 | `chat_completion_e2e::joiner_streams_through_mesh_and_attributes_peer` |
| `/v1/chat/completions` with tools — grammar-constrained path | ~ | P1 | `inference_adapter::guard_tests` covers tool-on-fast-slot guard; `adapter_translation_tests` covers shape; no integration test through the wire |
| `/v1/chat/completions` with tools — legacy marker path | ~ | P1 | Unit tests in `inference_adapter`; no integration |
| `/v1/chat/completions` peer routing (OICP) | ✓ | P1 | `chat_completion_e2e` (5 tests) |
| `LocalOnly` privacy short-circuit | ✓ | **P0** | `chat_completion_e2e::local_only_sharding_never_routes_to_peer` |
| Explicit `model` field overrides OICP | ✓ | P1 | `chat_completion_e2e::explicit_peer_model_id_routes_to_peer_without_oicp_envelope` |
| Unknown `model` errors (no silent substitution) | ✓ | P1 | `chat_completion_e2e::explicit_unknown_model_id_errors_instead_of_silent_substitution` |
| `/v1/embeddings` end-to-end | ✓ | P1 | `embeddings_e2e` (4 tests: single, batch, no-backend 503, empty 400) |
| `/v1/models` reflects loaded slots | ~ | P2 | `daemon::tests::register_local_model_slots_writes_info_for_all_three_slots` covers wiring; HTTP-surface untested |
| `/v1/responses` adapter (Responses API) | · | P1 | **Gap.** OpenAI Responses surface, used by `codex` clients — translation contract is in `routes_responses` |
| Streaming `finish_reason` carries through (`Length`, `Cancelled`, `ContentFilter`) | ✓ | **P0** | `finish_reason_streaming::{length_truncation_surfaces_length_on_final_chunk, content_filter_truncation_surfaces_content_filter_on_final_chunk, legacy_provider_default_impl_surfaces_stop}` |
| Tool envelope schema enforcement (`force_tool_calls`) | ~ | P1 | `tool_profile` unit tests; integration through `/v1/chat/completions` untested |
| Throughput observation → `InferenceReceived` ledger | ✓ | **P0** | `throughput_ledger_emission` |
| Zero-chunk peer route — no ledger event | ✓ | **P0** | `throughput_ledger_emission::peer_route_failure_without_chunks_does_not_emit_ledger_event` |
| Prompt compactor preserves message order + content | ~ | P1 | `prompt_compactor` unit tests; not integrated |

### C. Mesh coordination

| Capability | Coverage | Impact | Test / Next step |
|---|---|---|---|
| Gossip merge convergence (two AppStates) | ✓ | P1 | `gossip_integration::two_peers_converge_via_one_gossip_round` |
| Gossip auth (mesh_id / join_key_hash mismatch) | ✓ | **P0** | `gossip_auth::{wrong_mesh_id_rejects_with_401_and_no_mutation, wrong_join_key_hash_rejects_with_401_and_no_mutation, matching_credentials_accept_new_member_and_fire_hook}` |
| Gossip refresh of `hosted_corpora` | ✓ | P1 | `capabilities_published::gossip_round_publishes_live_hosted_corpora` |
| Offline decay of stale peer | ✓ | P2 | `gossip_integration::gossip_decays_stale_peer_to_offline` |
| Latency probing (UDP, EWMA) | ~ | P2 | `commonwealth-discovery` unit tests; no daemon-level |
| mDNS advertise + browse | · | P2 | **L4 territory.** In-process tests can't drive real multicast. Either accept the gap or build a mock multicast bus |
| `mesh.json` persistence hook on mutation | ✓ | **P0** | `join_handshake::valid_join_key_admits_new_member_and_fires_hook`, `daemon_wiring::with_mesh_mutation_hook_fires_on_gossip_delta` |
| `mesh.json` save/load round-trip via `persist::*` | ~ | **P0** | `persist` unit tests cover serde; no test that `save` → restart → `load` reconstructs a workable Mesh |
| Peer preference applied to outbound manifest fetch | ~ | **P0** | `peer_preferences` unit tests cover gossip exclusion + clamp; no test of the `X-Node-Id` stamp path through `MeshInferenceProvider` |
| Loopback parity across all loopback-only routers | ✓ | **P0** | `loopback_parity` (7 tests: 5 routers × non-loopback → 403, loopback negative control, missing-ConnectInfo fail-closed across all 5 routers) |
| `ConnectInfo` missing → fail-closed | ✓ | P1 | `loopback_guard::middleware_fails_closed_when_connect_info_missing` |
| `peer_inference_endpoints` URL synthesis under uniform-port assumption | · | P2 | Now config-derived (client_port) post-port-fix; uniform assumption documented in §10.1; no test of the rewrite shape |

### D. Knowledge

| Capability | Coverage | Impact | Test / Next step |
|---|---|---|---|
| `/v1/knowledge/search` local (corpus-engine + grounding) | ~ | P1 | `knowledge_served_e2e` exercises `/internal/knowledge/search` against a real `CorpusIndex`; the public-side `/v1/knowledge/search` with `corpus_engine=Some(...)` is exercised in `knowledge_fanout_e2e` as the joiner's local-first path |
| `/v1/knowledge/search` mesh fan-out + merge + rerank | ✓ | **P0** | `knowledge_fanout_e2e::{joiner_fans_out_to_peer_when_corpus_not_local, offline_peer_is_excluded_from_fan_out_plan}` |
| `/v1/knowledge/search` fan-out stamps `X-Node-Id` | ✓ | **P0** | **Fixed 2026-05-13.** `fanout_one_peer` now threads `self_id` through and sets `X-Node-Id: <self_hex>` on every outbound `/internal/knowledge/search` POST. Pinned by `knowledge_fanout_e2e::fan_out_stamps_x_node_id_so_peer_emits_ledger` which asserts A's `ContributionEmitter` records the expected `KnowledgeQueryServed { for_node: id_b, corpus_id, chunks_returned }` after a real B→A fan-out. |
| `/v1/knowledge/landscape_digest` | · | P1 | **Gap.** KnowledgeView is structurally local (§7) — a wire test pinning that 200 returns the digest *and* never leaks to the mesh path is the right shape |
| Canonical pull (peer fetches sharded corpus tar) | ✓ | P1 | `canonical_pull_e2e` (4 tests) |
| Knowledge query ledger emission (`KnowledgeQueryServed`) | ~ | **P0** | Spec §10 wires this in `routes_internal::knowledge_search`; no test that a fan-out actually emits one event per contributing corpus |
| Corpus install / update / remove via `MeshCorpusManager` | ~ | P1 | `commonwealth-knowledge` unit tests; daemon-level integration is missing |
| Embedding model info publication (collaborative ingest) | · | P2 | **Gap.** `set_local_embed_model` is called in `start_daemon`; collaborate handler reads it; round-trip uncovered |

### E. OICP / capabilities

| Capability | Coverage | Impact | Test / Next step |
|---|---|---|---|
| `build_self_manifest` shape (Fast + Slow + aliases + Code) | ✓ | P1 | `oicp_synthesis::self_manifest_tests` (6 tests) |
| `/oicp/v1/capabilities` HTTP serialization | · | P1 | **Gap.** Unit-tested at the builder, not over the wire |
| Manifest stamping with peer-preference multipliers | ✓ | **P0** | `peer_preference_manifest::{x_node_id_with_set_preference_halves_all_claim_affinities, x_node_id_for_unmatched_peer_does_not_modify_affinities, no_header_does_not_pick_up_any_stored_preference}` |
| Manifest cache TTL refresh on `MeshInferenceProvider` | ~ | P2 | `peer_inference` has the TTL constant; no test that an expired cache actually re-fetches |
| Peer quarantine / health weight scaling | ~ | P2 | `peer_inference` unit tests; no end-to-end where a failing peer is observed to back off |
| Effective in-flight scaling under health weight | ✓ | P2 | `peer_inference::effective_inflight` unit tests |

### F. Watched folders / corpus watch

| Capability | Coverage | Impact | Test / Next step |
|---|---|---|---|
| Register folder via `/internal/corpus/watch/register` | · | P1 | **Gap.** 14 routes, zero integration tests |
| Pause / resume / confirm-deletion state transitions | · | P1 | **Gap.** |
| Enable / disable / rebuild enrichment | · | P1 | **Gap.** |
| `details_handler` aggregates root + status + formats | · | P2 | **Gap.** 176-line handler with no test |
| Root management (add / remove) | · | P2 | **Gap.** |

### G. Project / code intelligence (`/v1/projects/*`)

| Capability | Coverage | Impact | Test / Next step |
|---|---|---|---|
| `/v1/projects` list | ~ | P3 | `project_http::tests` covers list-empty |
| `/v1/projects/register` happy + bad-id | ~ | P1 | `project_http::tests::register_rejects_empty_corpus_id` |
| `/v1/projects/{id}/rebuild` not-found | ~ | P2 | `project_http::tests::rebuild_unregistered_project_returns_404` |
| `/v1/projects/*` loopback enforcement | ✓ | **P0** | `loopback_parity::{project_http_rejects_non_loopback_via_list_projects, every_router_fails_closed_when_connect_info_absent}` |
| `Reindexer` state machine (rebuild_in_flight, graph_age) | ~ | P2 | Unit tests in `reindexer` and `projects::ProjectState` |

### H. Admin

| Capability | Coverage | Impact | Test / Next step |
|---|---|---|---|
| `ConfigDiff::diff` field semantics | ~ | P1 | `admin_http::tests::config_diff_*` |
| `/v1/admin/reload` HTTP route happy path | ✓ | **P0** | `admin_http::tests::reload_is_noop_when_nothing_changed` (spawns real HTTP listener via reqwest — lib tests at L2) |
| `/v1/admin/reload` swaps `InferenceProvider` | ✓ | **P0** | `admin_http::tests::reload_swaps_inference_provider_when_models_change` (asserts `ProviderFactory.build_provider` invoked + reloaded_fields populated) |
| `/v1/admin/reload` reports `restart_required: true` correctly | ✓ | P1 | `admin_http::tests::reload_port_change_requires_restart` |
| Loopback enforcement on `/v1/admin/reload` | ✓ | **P0** | `loopback_guard::enforce_localhost_accepts_loopback_rejects_others`, `admin_http::tests::enforce_localhost_rejects_non_loopback` |

### I. Auto-collaborate orchestration

| Capability | Coverage | Impact | Test / Next step |
|---|---|---|---|
| Partition planning across peers | ~ | P1 | `auto_ingest` unit tests; no integration |
| Handoff registration via `/internal/corpus/collaborate` | · | P1 | **Gap.** State machine has many transitions |
| Pull-based work queue reaper | · | P2 | **Gap.** Dormant-until-handoff design; never exercised end-to-end |
| Auto-collaborate peer compatibility filter (embed-model match) | · | P1 | **Gap.** Peers with mismatched embed dimensions must be rejected pre-handoff |

### J. MCP server

| Capability | Coverage | Impact | Test / Next step |
|---|---|---|---|
| `tools/list` dispatch | ✓ | P2 | `spec_gate_e2e::initialize_advertises_tools_list_changed_capability` |
| `tools/call` dispatch + audit trace | ~ | P1 | `mcp_router` lib tests; integration through MCP wire format |
| List-changed notifications via SSE | ✓ | P1 | `spec_gate_e2e::sse_pushes_tools_list_changed_via_get_mcp` |
| Spec-gating of tools by approval state | ✓ | P1 | `spec_gate_e2e::spec_creation_triggers_list_changed_notification_and_gates_tools_in` |
| `/mcp/message` backwards-compat POST | · | P3 | **Gap.** Legacy surface; risk of regression on rename |
| `/mcp/stats` | · | P3 | **Gap.** |
| Live wire pattern observation | ✓ | P1 | `pattern_observation_e2e::blast_then_build_writes_observed_note_via_live_mcp_wire` |

### K. Reading surface (`reading_http`)

| Capability | Coverage | Impact | Test / Next step |
|---|---|---|---|
| `/reading/chunks` (load + neighbor window) | ✓ | P2 | `reading_http_e2e::{get_chunk_returns_inserted_content, get_chunk_returns_404_for_unknown_corpus, get_chunk_returns_404_for_unknown_chunk_id, get_neighbors_returns_center_with_empty_prev_and_next_for_single_chunk_corpus}` |
| `/reading/atoms` card assembly | ~ | P2 | Atom formatter covered post-extraction by `reading_formatters` unit tests; HTTP surface for `/atoms/{id}` + `/atoms/{id}/elsewhere` still uncovered (atlas-enriched corpus required to exercise). |
| Cross-corpus link enumeration | · | P2 | **Gap.** |
| Loopback enforcement | · | **P0** | **Gap.** Same as project_http — needs the cross-router parity test |

### L. Apps platform

| Capability | Coverage | Impact | Test / Next step |
|---|---|---|---|
| `/v1/apps` list / install / uninstall | · | P2 | **Gap.** Mesh-app manifest gossip; future-facing surface |
| `/v1/apps/{id}/proxy` reverse proxy | · | P2 | **Gap.** |

### M. Contribution ledger

| Capability | Coverage | Impact | Test / Next step |
|---|---|---|---|
| `InferenceServed` emission on local serve | ~ | **P0** | Wired in `routes_inference::serve_local_*`; no end-to-end |
| `InferenceReceived` emission on peer-routed stream | ✓ | **P0** | `throughput_ledger_emission` |
| `KnowledgeQueryServed` per contributing corpus | ✓ | **P0** | `knowledge_served_e2e::{peer_request_emits_one_knowledge_query_served_per_contributing_corpus, local_origin_request_with_no_x_node_id_emits_nothing, unavailable_corpus_filter_emits_no_event_and_lists_unavailable}` covers the single-daemon emission contract; `knowledge_fanout_e2e::fan_out_stamps_x_node_id_so_peer_emits_ledger` covers the two-daemon path end-to-end after the X-Node-Id stamping fix. |
| `ShardTransferred` on `coordinate_merge` | ~ | P1 | `commonwealth-knowledge::ShardManager` unit tests; daemon-level untested |
| `StorageSnapshot` hourly emission | ✓ | P1 | `storage_snapshot_e2e::{first_tick_emits_only_mesh_shared_corpora_to_ledger, snapshot_emits_nothing_when_no_corpus_engine_attached}` |
| `current_contributions` aggregator | ~ | P2 | `commonwealth-state::contributions` unit tests |
| `peer_preferences` gossip exclusion | ✓ | **P0** | `commonwealth-state::peer_preferences::tests::gossip_excludes_peer_preferences_app_id` + `store::tests::all_entries_for_gossip_excludes_peer_preferences_namespace` |

### N. Persistence

| Capability | Coverage | Impact | Test / Next step |
|---|---|---|---|
| `mesh.json` serde round-trip | ~ | **P0** | `persist` unit tests |
| `mesh.json` save fires on every route mutation | ✓ | **P0** | `join_handshake`, `daemon_wiring` |
| `node_id` persistence across restart | ✓ | **P0** | `node_id_persistence::{node_id_survives_daemon_restart_against_same_data_dir, node_id_survives_mesh_leave_and_is_reused_on_next_create}` |
| `join_key.secret` persistence | · | P1 | **Gap.** Founder restart should keep the same join_key visible in `current_invite` |
| `RetentionGc` evicts past TTL | ~ | P2 | `commonwealth-state::RetentionGc` unit tests |
| `ContributionEmitter` self_node_id stamp on every event | ~ | **P0** | Unit-tested; no integration that the recorded events under load all carry the right origin |

### O. Workspace alignment (mesh-replicated `~/.claude/`)

| Capability | Coverage | Impact | Test / Next step |
|---|---|---|---|
| Replication via corpus-engine + gossip | ~ | P1 | `corpus-engine::sharding::merge_shards` unit tests |
| Projector newest-mtime LWW | ~ | P1 | `corpus-engine::alignment_projector` unit tests |
| Alignment corpus is structurally local (`mesh_sharing=false`) | ~ | **P0** | `corpus-engine` recipe-builder unit tests; daemon-level pin would catch a config drift |

### P. Failure modes

| Capability | Coverage | Impact | Test / Next step |
|---|---|---|---|
| Listener bind failure logged but daemon stays in valid state | · | P2 | **Gap.** `start_daemon` spawns the bind in `tokio::spawn`; if it fails the daemon still reports Running |
| mDNS register failure → daemon doesn't crash | ~ | P2 | Indirect: tests already pass with no real LAN multicast |
| Persistence write failure → request still succeeds | · | P1 | **Gap.** The `MeshMutationHook` swallows errors with a warn; degradation contract is "in-memory only" but no test |
| Gossip peer unreachable → decays to Offline without erroring | ✓ | P1 | `gossip_integration::gossip_decays_stale_peer_to_offline` |
| Bad `join_key` rejected with 401 + no mutation | ✓ | **P0** | `join_handshake::invalid_join_key_rejects_with_401_and_does_not_mutate` |
| Auto-leave gate refuses to clobber populated mesh | · | **P0** | **Gap.** `MeshError::AlreadyInPopulatedMesh` exists and is documented; the auto-leave gate that defends it has no test |
| Stream early termination (client drops mid-stream) | · | P1 | **Gap.** `ThroughputObservedStream::Drop` math depends on partial-state cleanup |
| Concurrent `set_inference_provider` swaps | · | P2 | **Gap.** Tests today wire then read; no concurrency |

---

## Top priority queue

Eighteen items ranked by impact × feasibility, with round-1+2 status
inline. Numbers in brackets index back to the matrix bucket.row.

1. ~~**[C.loopback parity] Single integration test walking every loopback-only route across all 7 mounted routers asserting non-loopback → 403.**~~ **Landed** as `loopback_parity` (7 tests). Cheap, high coverage, defended the §7 promise.
2. **[A.try_resume] `try_resume` → mesh reconstruction → first gossip round.** Pins the daemon-restart-overnight invariant. *Partial:* `node_id_persistence::node_id_survives_daemon_restart_against_same_data_dir` exercises `try_resume`; the "first gossip round after resume" half is still uncovered.
3. ~~**[H.admin reload] `/v1/admin/reload` end-to-end with provider swap.**~~ **Already covered** by `admin_http::tests::{reload_is_noop_when_nothing_changed, reload_swaps_inference_provider_when_models_change, reload_port_change_requires_restart}` — the lib tests spawn a real HTTP listener and use reqwest. Audit correction.
4. ~~**[N.node_id persistence] Daemon restart preserves `self_node_id`.**~~ **Landed** as `node_id_persistence` (2 tests).
5. ~~**[D.knowledge fan-out] `/v1/knowledge/search` two-daemon fan-out + merge + per-corpus ledger emission.**~~ **Landed** as `knowledge_fanout_e2e` (2 tests). The fan-out routes through correctly and offline-peer exclusion is pinned. **Caveat:** the ledger-emission half exposed a real bug — `fanout_one_peer` doesn't stamp `X-Node-Id`, so peer-side `KnowledgeQueryServed` stays silent during real fan-out traffic. Documented as a separate P0 cell; small follow-up fix.
6. ~~**[C.gossip auth] Foreign-mesh gossip payload doesn't pollute local state.**~~ **Landed** as `gossip_auth` (3 tests).
7. ~~**[E.manifest stamping] Peer-preference manifest stamping over `/oicp/v1/capabilities`.**~~ **Landed** as `peer_preference_manifest` (3 tests).
8. ~~**[B.finish_reason] End-to-end test that a `Length`-truncated stream surfaces `"length"` on the SSE chunk.**~~ **Landed** as `finish_reason_streaming` (3 tests, including `legacy_provider_default_impl_surfaces_stop` as negative control).
9. ~~**[M.KnowledgeQueryServed] Fan-out emits one ledger event per contributing corpus.**~~ **Landed** as `knowledge_served_e2e` (3 tests covering peer request, local-origin gating, and zero-chunk no-emission). See caveat under #5.
10. ~~**[M.StorageSnapshot] First-tick-immediate behavior + mesh_sharing filter.**~~ **Landed** as `storage_snapshot_e2e` (2 tests, real CorpusEngine with mesh-shared + local-only corpora).
11. ~~**[B.embeddings] `/v1/embeddings` smoke + multi-input batch.**~~ **Landed** as `embeddings_e2e` (4 tests).
12. **[B.responses adapter] `/v1/responses` translation contract.** `codex` clients depend on it.
13. **[F.corpus_watch happy path] Register → pause → resume → status round-trip.** Single test against the 14-route surface; closes the biggest single bucket gap.
14. **[F.corpus_watch enrichment] Enable → rebuild → details.** Sister test to (13).
15. **[I.auto-collaborate handoff] Register handoff → partition plan → state transitions.** Pins the state machine.
16. ~~**[K.reading_http] Chunks + atoms over wire.**~~ **Partially landed** as `reading_http_e2e` (4 tests covering chunk fetch happy + 404-corpus + 404-chunk + neighbors window). Atom-card endpoints (`/atoms/{id}` + `/atoms/{id}/elsewhere`) still uncovered — they require an atlas-enriched corpus and so didn't fit Round 4's scope.
17. ~~**[A.injection ordering] Tracing-capture test catching the silent-no-op `Arc::get_mut` failure.**~~ **Landed** as `injection_order` (3 tests).
18. **[B.tools end-to-end] Grammar-constrained tool call through `/v1/chat/completions`.** Currently unit-tested only.

After these, the matrix's remaining `·` cells are L4 territory
(mDNS, real GPU, launchd) or low-impact polish.

## Bugs surfaced by writing the tests

The priority queue's claim is "writing tests reveals bugs the
matrix didn't know about." Three so far across this work:

- **Round 4 — `X-Node-Id` not stamped on knowledge fan-out.**
  `routes_knowledge::fanout_one_peer` issued `/internal/knowledge/search`
  without the header, so the peer-side `KnowledgeQueryServed`
  emission path was permanently inactive for real fan-out
  traffic — the §10 intra-mesh accounting promise was silently
  broken in the most common case. **Fixed same day:** threaded
  `self_id` through the function, stamped `X-Node-Id: <self_hex>`
  on every outbound POST. Regression-pinned by
  `knowledge_fanout_e2e::fan_out_stamps_x_node_id_so_peer_emits_ledger`.
  Discovered while writing the two-daemon fan-out test — exactly
  the failure mode the matrix-driven test plan is designed to
  catch.
- **Pre-port-fix — `EmbeddedDaemon` hardcoded 9741/9742.**
  Caught by the desire to spin up two `EmbeddedDaemon`s in-process.
  Fixed in the pre-Round-1 batch.
- **Pre-Round-1 — `LedgerEmission` had `pub(crate)` fields**, so
  test impls of `PeerEndpointSource` couldn't construct it.
  Resolved by adding `LedgerEmission::new` constructor.

## Out-of-scope (intentional)

- **mDNS over real LAN.** Requires multicast; either accept the
  gap or build a mock multicast bus. Currently L4.
- **Real GPU inference quality.** Determinism-blocking. Manual smoke.
- **launchd / systemd service install.** Platform-specific. Manual.
- **Cross-machine real network (Tailscale, NAT).** Test harness
  exists in `commonwealth-test-harness::SimulatedMesh` for the
  standalone daemon — applies less cleanly here. Manual.
- **Standalone `commonwealth-daemon` paths that don't run in the
  embedded shape** (scheduler, orchestrator, `llama-server`/`rpc-server`
  spawn). Covered by `commonwealth-test-harness`'s `SimulatedMesh`.

## Conventions

- New tests go in `sovereign-mesh/tests/<concern>.rs` — one file
  per concern, multiple `#[tokio::test]`s per file.
- Real HTTP on `127.0.0.1:0` (ephemeral port). Never bind a
  literal port — collisions break parallel test runs.
- Mocked inference via small per-test `InferenceProvider` stub
  (the `LocalStub` / `StubProvider` pattern). See "Test harness
  consolidation" below.
- Mocked external HTTP via `axum::Router` mock servers (the
  `spawn_mock_peer` pattern in `chat_completion_e2e`).
- In-memory `MeshStore::in_memory()` for ledger / state.
- Refresh this doc in the same PR as the test landing.

## Test harness consolidation — **landed 2026-05-13**

`tests/common/mod.rs` (353 lines) exposes `TestProvider` (builder-
shaped configurable `InferenceProvider` stub), plus helpers
`empty_capabilities`, `member`, `member_with_last_seen`,
`solo_mesh`, `id_to_hex`, and `spawn_router`. Migrated:

- `injection_order` — `NoopProvider` → `TestProvider::new()`
- `embeddings_e2e` — `EmbedStub` → `TestProvider::new().with_embed_marker(...)`
- `daemon_wiring` — `StubProvider` → `TestProvider::new().with_complete_text("ok").with_stream_chunks(...).with_embed_marker(...)`
- `throughput_ledger_emission` — `LocalStub` → `TestProvider::new().with_model_id("qwen2.5-3b-instruct-q4_k_m")`
- `chat_completion_e2e` — `LocalStub` (×5 call sites) → `local_byom()` thin wrapper around `TestProvider`
- `gossip_auth` — helpers only (`member_with_last_seen`, `spawn_router`)
- `peer_preference_manifest` — `ManifestProvider` → `TestProvider::new().with_model_id("manifest-stub")`; `id_to_hex` also extracted
- `finish_reason_streaming` — `FixedFinishProvider` + `LegacyStreamProvider` → `TestProvider::new().with_typed_frames(...)` + `TestProvider::new().with_stream_chunks(...)`

**Not migrated** (different stub trait): `spec_gate_e2e` and
`pattern_observation_e2e` use `ToolRegistry` stubs (`StubTool`),
not `InferenceProvider` — different concern, separate extraction
story.

**Outcome:** all 287 tests still green. ARCH §10.3 threshold
discharged; new tests in subsequent rounds can declare
`TestProvider::new().with_*(...)` instead of hand-rolling a
40+ LOC stub.

**TestProvider design notes** (worth knowing before extending):

- All methods return `NotImplemented` by default. Each `with_*`
  builder opts into specific behaviour. This is intentional: a
  future regression that starts calling an unconfigured method
  surfaces as a clear error, not silent success.
- `with_typed_frames(...)` overrides `complete_stream_with_finish`
  directly (used to pin non-Stop finish reasons). When None, the
  default impl runs on top of `complete_stream` — that's why the
  default-impl logic is **reproduced inline** rather than
  delegated, to avoid infinite recursion.
- Bar for adding a new `with_*` method: two callers need it.
  Avoid premature flexibility per ARCH §10.3.
