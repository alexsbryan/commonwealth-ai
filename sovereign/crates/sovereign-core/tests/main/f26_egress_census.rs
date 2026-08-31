// SPDX-License-Identifier: AGPL-3.0-or-later
//! F26 — the egress-boundary census, enforced as a build gate
//! (bar `dr-egress`, order deep-research-t2a).
//!
//! The bar's instrument (PLAN.md F26): "every remote client
//! construction routes through the boundary, enforced as a build
//! gate". This file IS the census: a deterministic (no model)
//! enumeration of every HTTP-client construction site in the
//! workspace's production `src/` trees, checked against a reviewed
//! registry. A new construction site anywhere fails the gate until
//! it is registered and classified — the review moment is the
//! commit, never a silent pass.
//!
//! Scope decisions (reviewed at landing, order deep-research-t2a):
//! - Scan scope: every workspace member's `src/` tree (the root
//!   Cargo.toml `members` list — a new member auto-enters the
//!   scan). `tests/`, `examples/`, `benches/` and `build.rs` are
//!   outside the scope: the census guards PRODUCTION construction,
//!   which is what F26's "remote client construction" means.
//! - The boundary guards egress to THIRD-PARTY endpoints: remote
//!   model providers (RemotePayload) and search engines
//!   (QueryEgress). Both classes are FORBIDDEN outside the ONE
//!   boundary module (sovereign-core/src/egress.rs).
//! - Mesh / peer / pod traffic (Mesh) is the estate's own transport
//!   — its own auth and custody class; daemon-local traffic
//!   (LocalDaemon) never leaves the machine; inbound-only sites
//!   (InboundOnly: downloads, fetches, probes) carry content IN,
//!   never estate payloads out; operator-configured endpoints
//!   (OperatorSurface: MCP servers, CalDAV) are operator-owned
//!   targets; test-only sites (TestOnly) live in test fixtures.
//! - Counts: per-file construction-site counts are part of the
//!   registry. A count drift fails the gate so a NEW site is a
//!   review moment, never a silent pass.
//!
//! At HEAD (the red, before the boundary exists) the census counts
//! five egress-class construction sites outside the boundary, the
//! enrich providers path named first:
//!   - sovereign/crates/sovereign-cli-llm/src/enrich_cmd/inference_client.rs
//!     (RemotePayload — the `--provider` chat client)
//!   - sovereign/crates/sovereign-core/src/deep_research/port.rs
//!     (QueryEgress — the web_search client)
//!   - sovereign/crates/sovereign-tools/src/knowledge_lookup/mod.rs
//!     (QueryEgress — the web-escalation client)
//!   - studio/crates/sovereign-tools-base/src/web/mod.rs
//!     (QueryEgress — the search tool's default_client)
//!   - sovereign/crates/sovereign-desktop/src-tauri/src/commands/
//!     conversation.rs (the Search-the-web card's client — carried
//!     at the red as `LocalDaemon 1`; corrected at landing when the
//!     re-home review saw the site dispatch External queries)
//!
//! At the landing (the boundary + this census in ONE commit) every
//! one of those sites routes through BOUNDARY_MODULE and the rows
//! read Boundary (egress.rs, 2 sites) / LocalDaemon / InboundOnly —
//! the census then reads ZERO egress-class rows outside the boundary.
//!
//! R-6 (bar `dr-budget-one-decider`) rides in the same file: the
//! identifiers of every other budget decider in the workspace
//! (`budget_allows`, `decrement_budget`, `BudgetView`) must be
//! absent from production src trees, and the ONE run-scoped
//! fail-closed SpendDecider must exist in sovereign-core.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

/// The ONE egress boundary module: every remote-model call and every
/// search-query egress is constructed through it (bar dr-egress).
const BOUNDARY_MODULE: &str = "sovereign/crates/sovereign-core/src/egress.rs";

/// The ONE run-scoped fail-closed budget decider (bar
/// dr-budget-one-decider). Frontier-key spend is declared here and
/// inert until t2b opens the judge role.
const SPEND_DECIDER_MODULE: &str = "sovereign/crates/sovereign-core/src/deep_research/budget.rs";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Class {
    /// Constructed inside the ONE boundary module — legal by
    /// definition. The only class that may carry RemotePayload or
    /// QueryEgress construction.
    Boundary,
    /// Sends estate content to a REMOTE model provider. FORBIDDEN
    /// outside BOUNDARY_MODULE.
    RemotePayload,
    /// Sends search queries to an external search engine. FORBIDDEN
    /// outside BOUNDARY_MODULE.
    QueryEgress,
    /// Mesh / peer / pod traffic — the estate's own transport (own
    /// auth, custody class peer). Not third-party egress.
    Mesh,
    /// Loopback / host-daemon traffic — never leaves the machine.
    LocalDaemon,
    /// Content IN (downloads, fetches, probes) — no estate payload
    /// travels out.
    InboundOnly,
    /// Operator-configured endpoints (MCP servers, CalDAV).
    OperatorSurface,
    /// Test-only construction (`#[cfg(test)]` or test fixtures).
    TestOnly,
}

impl Class {
    fn as_str(self) -> &'static str {
        match self {
            Class::Boundary => "Boundary",
            Class::RemotePayload => "RemotePayload",
            Class::QueryEgress => "QueryEgress",
            Class::Mesh => "Mesh",
            Class::LocalDaemon => "LocalDaemon",
            Class::InboundOnly => "InboundOnly",
            Class::OperatorSurface => "OperatorSurface",
            Class::TestOnly => "TestOnly",
        }
    }
}

/// Every HTTP-client construction site in the workspace's production
/// src trees, reviewed 2026-08-17 (order deep-research-t2a). Each
/// row: (repo-relative path, the file's most-privileged traffic
/// class, construction-site count).
///
/// A row's count must equal what the census scan finds. When the
/// egress boundary landed (order deep-research-t2a), the egress-class
/// rows below changed class to their remaining local sites
/// (LocalDaemon / InboundOnly) or disappeared (their construction
/// moved into the boundary), and BOUNDARY_MODULE gained the Boundary
/// row — in the SAME commit as the boundary code, per the
/// review-moment contract.
#[rustfmt::skip]
const REGISTRY: &[(&str, Class, usize)] = &[
    // ---- the ONE egress boundary ----
    // The only legal construction site for remote-model (RemotePayload)
    // and search-query (QueryEgress) clients: search_client() (30s)
    // and model_client(timeout). Everything below is Local / Mesh /
    // InboundOnly / OperatorSurface / TestOnly.
    ("sovereign/crates/sovereign-core/src/egress.rs", Class::Boundary, 2),
    // deep_research/port.rs: the live `ResearchPort` — the rung-2/3
    // acquisition surface. Its web_search client is built by
    // BOUNDARY_MODULE (egress.rs `search_client`), and the query
    // egress passes the release gate (consent grant, default-deny),
    // so the one site counted here is the /v1/models probe against
    // the host daemon. Lifted out of sovereign-cli so the desktop
    // stops spawning the CLI to reach it — the classification did not
    // change with the crate.
    ("sovereign/crates/sovereign-core/src/deep_research/port.rs", Class::LocalDaemon, 1),
    // sovereign-turn-client: the client half of the turn protocol, minted
    // 2026-08-25 (TOPOLOGY §10 phase 6). One `reqwest::Client` for
    // `POST /v1/conversations` + the conversation-end call; the turn itself
    // is a WebSocket. Loopback to this host's own daemon — `turn_http` is
    // loopback-only at both layers and refuses anything else — so the class
    // is LocalDaemon, not egress. Nothing here leaves the machine.
    ("sovereign/crates/sovereign-turn-client/src/lib.rs", Class::LocalDaemon, 1),

    // ---- sovereign-mesh: the estate's own transport (Mesh) ----
    // Peer-to-peer / daemon-mesh HTTP; own auth + custody class.
    // Not third-party egress — the boundary does not gate the estate's
    // own substrate.
    // 15 -> 17 (2026-08-27): the `mesh_switch` handler and the two new rotate
    // tests each construct a client for loopback calls to our own daemon.
    // Class unchanged — mesh transport, never third-party egress.
    ("sovereign/crates/sovereign-mesh/src/mesh_http.rs", Class::Mesh, 17),
    ("sovereign/crates/sovereign-mesh/src/rpc_warm_http.rs", Class::Mesh, 7),
    ("sovereign/crates/sovereign-mesh/src/worker_http.rs", Class::Mesh, 6),
    // 5 -> 7 (2026-08-23): the two reload-diff regression tests
    // (`reload_applies_a_context_size_change_without_a_restart`,
    // `reload_applies_a_code_slot_change_without_a_restart`) each build a
    // client to POST /v1/admin/reload. Inline `#[cfg(test)]` lives in a
    // src/ file, so the census counts it; the class is unchanged — this is
    // loopback admin traffic to our own daemon, never third-party egress.
    ("sovereign/crates/sovereign-mesh/src/admin_http.rs", Class::Mesh, 7),
    ("sovereign/crates/sovereign-mesh/src/project_http.rs", Class::Mesh, 4),
    ("sovereign/crates/sovereign-mesh/src/model_fetch.rs", Class::Mesh, 4),
    ("sovereign/crates/sovereign-mesh/src/loopback_guard.rs", Class::Mesh, 3),
    ("sovereign/crates/sovereign-mesh/src/peer_inference.rs", Class::Mesh, 2),
    // setup_cmd/terminal.rs (2026-08-30, the `terminal` node class; 1 -> 3 on
    // 2026-08-31 when `--terminal` learned to take a join link). THREE clients,
    // and the traffic class is unchanged — every destination is either this
    // machine or a node on the operator's own mesh:
    //   1. the address path's client, unchanged: `GET /status` for the entry
    //      node's embed model id, then one `POST /v1/chat/completions` to prove
    //      a turn comes back served, both against an address the operator typed;
    //   2. the join path's client, same two probes against the member the mesh
    //      says holds the models — an address nobody typed, which is the point
    //      of the change, but still a peer of a mesh this node just joined;
    //   3. `daemon_is_listening`, a `GET http://127.0.0.1:9741/v1/models` that
    //      never leaves the box. Setup joins the mesh in-process and needs the
    //      client port, so it refuses to run beside a live daemon rather than
    //      producing the split-brain `mesh_cmd::cmd_join` documents.
    // The only payload that leaves in any of the three is the same fixed
    // four-word probe prompt, with no estate content in it.
    ("sovereign/crates/sovereign-cli-daemon/src/setup_cmd/terminal.rs", Class::Mesh, 3),
    // guest_lender.rs (2026-08-28, order mesh-guest-grant): resolving a model
    // id to a node this one holds a GUEST GRANT with, so a guest's turn runs
    // on their own daemon and only the completion crosses. ONE site — the
    // client that fetches the lender's `/v1/models` under the bearer, which
    // is the authority on what the grant buys. The dispatch itself constructs
    // nothing: it reuses `MeshInferenceProvider::http`.
    //
    // Mesh, and the judgement is worth stating because a lender is NOT a mesh
    // member and the class name reads as if it should be. Three checks:
    //   - NOT third-party. The boundary guards egress to third-party
    //     endpoints — commercial model providers and search engines (see the
    //     module header; the RemotePayload exemplar is the `--provider` chat
    //     client). A lender is another Sovereign node speaking the same
    //     client API. Mesh peers are equally "someone else's machine"; what
    //     separates Mesh from RemotePayload here is a Sovereign counterparty
    //     the operator has an explicit trust relationship with, not whose
    //     hardware it is.
    //   - The estate's own transport, own auth. It rides GUEST_ALPN through
    //     `sovereign_mesh::guest_tunnel` on an encrypted mesh, and the
    //     credential is a grant that node itself issued and can revoke.
    //   - The destination is operator-chosen and not request-derived. It
    //     comes from `guest.json`, written only by `svrn mesh use`. No
    //     parameter of any function here names a host, so no caller — and no
    //     prompt — can aim it.
    // What this row does NOT cover: widening a grant beyond
    // `/v1/chat/completions` + `/v1/models`, or letting a request parameter
    // choose the lender. Either is a re-classification, not a count bump.
    ("sovereign/crates/sovereign-mesh/src/guest_lender.rs", Class::Mesh, 1),
    ("sovereign/crates/sovereign-mesh/src/join.rs", Class::Mesh, 2),
    ("sovereign/crates/sovereign-mesh/src/daemon.rs", Class::Mesh, 2),
    ("sovereign/crates/sovereign-mesh/src/auto_ingest.rs", Class::Mesh, 2),
    ("sovereign/crates/sovereign-mesh/src/worker_subprocess_runner.rs", Class::Mesh, 1),
    ("sovereign/crates/sovereign-mesh/src/worker_inference_proxy.rs", Class::Mesh, 1),
    ("sovereign/crates/sovereign-mesh/src/landscape_digest_client.rs", Class::Mesh, 1),
    ("sovereign/crates/sovereign-mesh/src/knowledge_client.rs", Class::Mesh, 1),
    ("sovereign/crates/sovereign-mesh/src/gossip.rs", Class::Mesh, 1),
    ("sovereign/crates/sovereign-mesh/src/canonical_pull.rs", Class::Mesh, 1),

    // ---- sovereign-desktop: the host daemon on :9741 (LocalDaemon) ----
    // All desktop commands talk to the local daemon's /internal/*
    // surfaces; never third-party egress. (conversation.rs once
    // carried a Search-the-web client here, counted LocalDaemon 1 at
    // the red — the re-home review at landing found it dispatched
    // External queries and its construction moved into the boundary;
    // the row is gone with the site.)
    ("sovereign/crates/sovereign-desktop/src-tauri/src/commands/corpus_install.rs", Class::LocalDaemon, 8),
    ("sovereign/crates/sovereign-desktop/src-tauri/src/local_corpus_commands.rs", Class::LocalDaemon, 7),
    ("sovereign/crates/sovereign-desktop/src-tauri/src/commands/contribution.rs", Class::LocalDaemon, 7),
    ("sovereign/crates/sovereign-desktop/src-tauri/src/commands/budget.rs", Class::LocalDaemon, 6),
    ("sovereign/crates/sovereign-desktop/src-tauri/src/import_commands.rs", Class::LocalDaemon, 2),
    ("sovereign/crates/sovereign-desktop/src-tauri/src/commands/hardware.rs", Class::LocalDaemon, 2),
    ("sovereign/crates/sovereign-desktop/src-tauri/src/watched_folder_commands.rs", Class::LocalDaemon, 1),
    ("sovereign/crates/sovereign-desktop/src-tauri/src/recipe_commands.rs", Class::LocalDaemon, 1),
    ("sovereign/crates/sovereign-desktop/src-tauri/src/mobile_host_setup.rs", Class::LocalDaemon, 1),
    ("sovereign/crates/sovereign-desktop/src-tauri/src/mesh_commands.rs", Class::LocalDaemon, 1),
    ("sovereign/crates/sovereign-desktop/src-tauri/src/commands/reading.rs", Class::LocalDaemon, 1),
    ("sovereign/crates/sovereign-desktop/src-tauri/src/commands/models.rs", Class::InboundOnly, 1),
    ("sovereign/crates/sovereign-desktop/src-tauri/src/commands/diagnostics.rs", Class::LocalDaemon, 1),
    ("sovereign/crates/sovereign-desktop/src-tauri/src/commands/config_setup.rs", Class::LocalDaemon, 1),
    ("sovereign/crates/sovereign-desktop/src-tauri/src/collaborate_commands.rs", Class::LocalDaemon, 1),
    ("sovereign/crates/sovereign-desktop/src-tauri/src/bootstrap.rs", Class::LocalDaemon, 1),
    ("sovereign/crates/sovereign-desktop/src-tauri/src/attach_watch.rs", Class::LocalDaemon, 1),

    // ---- sovereign-cli-llm ----
    // R-5's named path: the enrich --provider dispatch. The chat
    // client (complete_inner → complete_openai_compatible /
    // complete_anthropic) moved into BOUNDARY_MODULE (egress.rs
    // model_client) with the boundary — a remote provider now passes
    // the release gate (default custody Personal, no grant →
    // typed refusal). The file's three remaining sites are local:
    // the embed one-shot client + two /v1/models probes.
    ("sovereign/crates/sovereign-cli-llm/src/enrich_cmd/inference_client.rs", Class::LocalDaemon, 3),
    // 7 -> 10 (2026-08-27): `mesh rotate`, `mesh leave` and `mesh switch` each
    // now prefer the running daemon over an in-process fallback (the `cmd_join`
    // pattern), so each builds a client for 127.0.0.1 loopback. `mesh rotate`
    // in particular HAD to move: an offline rotation was reverted by the next
    // gossip round. Class unchanged — loopback to our own daemon.
    ("sovereign/crates/sovereign-cli-llm/src/mesh_cmd.rs", Class::Mesh, 10),
    // NEW 2026-08-28: `svrn mesh forget-member`, the repair for an
    // endpoint-key collision, posts to the running daemon's
    // /v1/mesh/forget-member. Class Mesh — 127.0.0.1 loopback to our own
    // daemon, and deliberately so: the roster lives in the daemon's memory,
    // and an offline edit to mesh.json would be reverted by the next gossip
    // round exactly as `mesh rotate`'s was. One site; the collision WARNING
    // that names this command is pure rendering and builds no client.
    (
        "sovereign/crates/sovereign-cli-llm/src/mesh_member_cmd.rs",
        Class::Mesh,
        1,
    ),
    // NEW 2026-08-27: `svrn mesh grant` / `svrn mesh use`, the two ends of an
    // ephemeral guest link. One shared `http_client()` builder serves both
    // directions — loopback to our own daemon to mint/revoke/list, and one
    // outbound GET to the ISSUING node's `/v1/models` so `mesh use` can refuse
    // a dead link before storing it. Both are Commonwealth nodes, so Mesh is
    // the honest class: no third-party model or search traffic passes here,
    // and nothing on this path may construct a RemotePayload/QueryEgress
    // client (that stays in the boundary).
    ("sovereign/crates/sovereign-cli-llm/src/mesh_guest.rs", Class::Mesh, 1),
    ("sovereign/crates/sovereign-cli-llm/src/search_gym_cmd/mod.rs", Class::LocalDaemon, 3),
    ("sovereign/crates/sovereign-cli-llm/src/recipe_agent_live_trial.rs", Class::LocalDaemon, 3),
    ("sovereign/crates/sovereign-cli-llm/src/mesh_bench.rs", Class::Mesh, 3),
    ("sovereign/crates/sovereign-cli-llm/src/remote_gguf.rs", Class::InboundOnly, 2),
    ("sovereign/crates/sovereign-cli-llm/src/corpus_watch_cmd.rs", Class::LocalDaemon, 2),
    ("sovereign/crates/sovereign-cli-llm/src/chat_cmd/bootstrap.rs", Class::LocalDaemon, 2),
    ("sovereign/crates/sovereign-cli-llm/src/workflow_cmd.rs", Class::LocalDaemon, 1),
    ("sovereign/crates/sovereign-cli-llm/src/solve_cmd.rs", Class::LocalDaemon, 1),
    ("sovereign/crates/sovereign-cli-llm/src/recipe_cmd.rs", Class::LocalDaemon, 1),
    ("sovereign/crates/sovereign-cli-llm/src/pipeline_cmd.rs", Class::LocalDaemon, 1),
    ("sovereign/crates/sovereign-cli-llm/src/mobile_cmd.rs", Class::LocalDaemon, 1),
    ("sovereign/crates/sovereign-cli-llm/src/mesh_travel.rs", Class::Mesh, 1),
    ("sovereign/crates/sovereign-cli-llm/src/knowledge_gym_cmd/mod.rs", Class::LocalDaemon, 1),
    ("sovereign/crates/sovereign-cli-llm/src/corpus_snapshot_cmd.rs", Class::InboundOnly, 1),
    ("sovereign/crates/sovereign-cli-llm/src/corpus_cmd/inventory.rs", Class::LocalDaemon, 1),
    ("sovereign/crates/sovereign-cli-llm/src/bench_cmd/uap.rs", Class::LocalDaemon, 1),
    ("sovereign/crates/sovereign-cli-llm/src/bench_cmd/model_resolve.rs", Class::LocalDaemon, 1),
    ("sovereign/crates/sovereign-cli-llm/src/bench_cmd/desktop_bridge.rs", Class::LocalDaemon, 1),
    ("sovereign/crates/sovereign-cli-llm/src/bench_cmd/atlas.rs", Class::LocalDaemon, 1),
    ("sovereign/crates/sovereign-cli-llm/src/alignment_cmd.rs", Class::LocalDaemon, 1),

    // ---- sovereign-cli-dev (all LocalDaemon) ----
    // `doc_fetcher.rs` held this family's only InboundOnly row until
    // 2026-08-26, when it was deleted with `honesty.rs` as a closed pair
    // that had been unreachable since `de34eb36` (commit 2bbcb480). The
    // row outlived the file by one commit and the census caught it as a
    // STALE ROW — which is the census working.
    // code_cmd 4 -> 3 (2026-08-20): the fourth site was
    // `build_daemon_embed_fn`'s /v1/models probe, which left with the rest of
    // `svrn code index` for sovereign-cli-shared::code_index. The three that
    // remain are cmd_facts' http client and cmd_watch's two.
    ("sovereign/crates/sovereign-cli-dev/src/code_cmd.rs", Class::LocalDaemon, 3),
    ("sovereign/crates/sovereign-cli-dev/src/tools_cmd/registry.rs", Class::LocalDaemon, 3),
    // `svrn ring` talks to ONE address: `127.0.0.1:<daemon client_port>`, for
    // the rail routes and the guest-grant mint. Nothing a ring app writes
    // leaves the machine through this client — replication is the daemon's
    // own peer traffic (`ring_sync`), on the mesh class.
    ("sovereign/crates/sovereign-cli-llm/src/ring_cmd/mod.rs", Class::LocalDaemon, 1),
    // 2 -> 3 on 2026-08-21 (nc-27): `daemon_get` MOVED here from
    // `project_cmd/registry_watch.rs` when that file was deleted as an
    // unreachable fork. Same loopback client, same class — a relocation,
    // not a new egress site.
    ("sovereign/crates/sovereign-cli-dev/src/project_cmd/mod.rs", Class::LocalDaemon, 3),
    ("sovereign/crates/sovereign-cli-dev/src/plan_enricher.rs", Class::LocalDaemon, 2),
    ("sovereign/crates/sovereign-cli-dev/src/code_map.rs", Class::LocalDaemon, 2),
    ("sovereign/crates/sovereign-cli-dev/src/atos_cmd/doctor.rs", Class::LocalDaemon, 2),
    ("sovereign/crates/sovereign-cli-dev/src/drift_cmd_orchestrator.rs", Class::LocalDaemon, 1),
    // refactor_cmd/label_model: the name-group adjudication pass. One client,
    // pinned to the local daemon — it posts Rust source snippets and the
    // code-intel descriptions of the types under judgement, which are estate
    // content, so LocalDaemon is the class that keeps them on the machine. A
    // future `--daemon-url` pointing off-box would be the review moment, not
    // a count change.
    ("sovereign/crates/sovereign-cli-dev/src/refactor_cmd/label_model.rs", Class::LocalDaemon, 1),
    ("sovereign/crates/sovereign-cli-dev/src/design_session.rs", Class::LocalDaemon, 1),
    ("sovereign/crates/sovereign-cli-dev/src/code_capability_graph.rs", Class::LocalDaemon, 1),
    ("sovereign/crates/sovereign-cli-dev/src/atos_cmd/run.rs", Class::LocalDaemon, 1),
    ("sovereign/crates/sovereign-cli-dev/src/atos_cmd/replay.rs", Class::LocalDaemon, 1),
    ("sovereign/crates/sovereign-cli-dev/src/atos_cmd/ab.rs", Class::LocalDaemon, 1),

    // ---- sovereign-cli-daemon (LocalDaemon — daemon self-control) ----
    // `doctor_cmd.rs` was split along its three declared layers; the three
    // construction sites moved with the code they probe with. Same class,
    // same total, new paths.
    ("sovereign/crates/sovereign-cli-daemon/src/doctor_cmd/probe.rs", Class::LocalDaemon, 2),
    (
        "sovereign/crates/sovereign-cli-daemon/src/doctor_cmd/checks_freshness.rs",
        Class::LocalDaemon,
        1,
    ),
    ("sovereign/crates/sovereign-cli-daemon/src/daemon_cmd/lifecycle.rs", Class::LocalDaemon, 3),
    ("sovereign/crates/sovereign-cli-daemon/src/setup_cmd/fim.rs", Class::LocalDaemon, 2),
    ("sovereign/crates/sovereign-cli-daemon/src/setup_cmd/finish.rs", Class::LocalDaemon, 1),
    ("sovereign/crates/sovereign-cli-daemon/src/model_cmd.rs", Class::LocalDaemon, 1),

    // ---- sovereign-cli ----
    ("sovereign/crates/sovereign-cli/src/project_registry.rs", Class::LocalDaemon, 2),
    ("sovereign/crates/sovereign-cli/src/update_cmd.rs", Class::InboundOnly, 1),
    ("sovereign/crates/sovereign-cli/src/session_cmd.rs", Class::LocalDaemon, 1),
    ("sovereign/crates/sovereign-cli/src/serve_cmd.rs", Class::LocalDaemon, 1),
    ("sovereign/crates/sovereign-cli/src/project_init/mod.rs", Class::LocalDaemon, 1),
    ("sovereign/crates/sovereign-cli/src/notes_cmd.rs", Class::LocalDaemon, 1),
    // code_index_cmd.rs has no row since 2026-08-20: the dispatcher's copy of
    // `svrn code index` (and with it the probe client) moved to
    // sovereign-cli-shared::code_index; what is left here is a 42-line
    // subcommand shim that constructs nothing.

    // ---- sovereign-cli-shared (LocalDaemon: daemon MCP proxy + project-local) ----
    ("sovereign/crates/sovereign-cli-shared/src/mcp_client.rs", Class::LocalDaemon, 3),
    // code_index.rs (2026-08-20): `svrn code index` was two copies, one per
    // binary, and one of them carried a live `--help` defect; converging it put
    // `build_daemon_embed_fn` here. CLASSIFIED FRESH, not carried across, since
    // a client constructor moving from a leaf binary into a SHARED library is a
    // different reachability story on its face. Three checks, and all three say
    // LocalDaemon is still right:
    //   - Destination is pinned, not passed. The one site builds a 2s-timeout
    //     probe for `format!("http://localhost:{port}/v1")/models`, where only
    //     the PORT comes from config. No parameter of the function names a
    //     host, so no caller can aim it off-box. The classes in this registry
    //     are about where the bytes go, and these go to loopback.
    //   - It carries nothing out. The site is a bare GET liveness probe; no
    //     estate content, not even a query, is in the request.
    //   - Reachability did not actually widen. `code_index` is behind the
    //     `code-index` feature, enabled by exactly `sovereign-cli` (via
    //     `code-intel`) and `sovereign-cli-dev` — the same two binaries that
    //     held the code before. The other two crates depending on this one
    //     (sovereign-cli-daemon, sovereign-cli-llm) leave the feature off, so
    //     the module is not compiled into them at all.
    // What this row does NOT guarantee: if someone later gives
    // `build_daemon_embed_fn` an endpoint parameter, the count stays 1 and this
    // census stays green. The pinned-localhost literal is the invariant; a
    // change to it is the review moment, not a change to the count.
    ("sovereign/crates/sovereign-cli-shared/src/code_index.rs", Class::LocalDaemon, 1),

    // ---- sovereign-tools ----
    // knowledge_lookup: the tool-registry web-search evidence path —
    // its client construction moved into BOUNDARY_MODULE (egress.rs
    // search_client) with the boundary, and the query egress passes
    // the release gate (user-formed-query clause — the user's own
    // question). No row: the file's construction sites are zero.
    // sec_edgar: the SEC filings acquirer's client (order
    // sec-filings-last-mile). InboundOnly on the same reading as every
    // other acquirer (corpus-engine/src/acquirers/*): it FETCHES from
    // data.sec.gov and www.sec.gov — company_tickers.json, submissions,
    // the 10-K primary document, companyfacts — and no estate content
    // travels out. The only outbound datum is the ticker the user typed
    // and the contact address the recipe declares in its User-Agent
    // (`[parameters.contact]`, visible and editable precisely because it
    // is sent on the user's behalf); neither is corpus content, and SEC
    // is a public-record endpoint rather than a model provider or a
    // search engine.
    ("sovereign/crates/sovereign-tools/src/sec_edgar.rs", Class::InboundOnly, 1),
    ("sovereign/crates/sovereign-tools/src/notes/diff_extract_backend.rs", Class::LocalDaemon, 1),
    ("sovereign/crates/sovereign-tools/src/local_corpus/ocr/cleanup.rs", Class::LocalDaemon, 1),
    ("sovereign/crates/sovereign-tools/src/corpus/manager.rs", Class::InboundOnly, 1),
    ("sovereign/crates/sovereign-tools/src/catalog_ingest.rs", Class::LocalDaemon, 1),
    ("sovereign/crates/sovereign-tools/src/calendar.rs", Class::OperatorSurface, 1),

    // ---- sovereign-server (LocalDaemon — API server → host daemon) ----
    ("sovereign/crates/sovereign-server/src/startup.rs", Class::LocalDaemon, 1),
    ("sovereign/crates/sovereign-server/src/reciprocity.rs", Class::LocalDaemon, 1),
    ("sovereign/crates/sovereign-server/src/activity.rs", Class::LocalDaemon, 1),

    // ---- sovereign-inference (InboundOnly: range-resumed model downloads) ----
    ("sovereign/crates/sovereign-inference/src/setup_planner.rs", Class::InboundOnly, 1),

    // ---- sovereign-gliner (InboundOnly: HuggingFace model download) ----
    ("sovereign/crates/sovereign-gliner/src/gliner_ner.rs", Class::InboundOnly, 1),

    // ---- sovereign-eval (LocalDaemon — eval against the host daemon) ----
    ("sovereign/crates/sovereign-eval/src/tool_grader.rs", Class::LocalDaemon, 1),
    ("sovereign/crates/sovereign-eval/src/manifest.rs", Class::LocalDaemon, 1),
    ("sovereign/crates/sovereign-eval/src/judge.rs", Class::LocalDaemon, 1),
    ("sovereign/crates/sovereign-eval/src/cognitive/runner.rs", Class::LocalDaemon, 1),

    // ---- sovereign-agent-bench (LocalDaemon — bench against the host daemon) ----
    ("sovereign/crates/sovereign-agent-bench/src/runners/native.rs", Class::LocalDaemon, 2),
    ("sovereign/crates/sovereign-agent-bench/src/runners/bare_metal.rs", Class::LocalDaemon, 2),
    ("sovereign/crates/sovereign-agent-bench/src/judge.rs", Class::LocalDaemon, 1),
    ("sovereign/crates/sovereign-agent-bench/src/cli/replay.rs", Class::LocalDaemon, 1),

    // ---- commonwealth-tdd (LocalDaemon — TDD loop against the daemon) ----
    ("sovereign/crates/commonwealth-tdd/src/backend.rs", Class::LocalDaemon, 2),

    // ---- sovereign-compute ----
    // client: loopback back to the host daemon; supervisor: heartbeat
    // to compute pods (estate infrastructure, own auth).
    ("sovereign/crates/sovereign-compute/src/client.rs", Class::LocalDaemon, 1),
    ("sovereign/crates/sovereign-compute/src/supervisor.rs", Class::Mesh, 1),

    // ---- oicp-client (Mesh — OICP client → a daemon, ours or a peer's) ----
    // 2 -> 3 on 2026-08-31: `RemoteApiProvider::dynamic`, the constructor for a
    // provider whose endpoint is RESOLVED per call rather than fixed (a
    // `terminal` node bound to its entry node by mesh identity).
    //
    // RECLASSIFIED LocalDaemon -> Mesh in the same change, and the row was
    // already imprecise before it: `LocalDaemon` means "never leaves the
    // machine", but `provider_for_peer` has always built these providers
    // against a PEER's address. `dynamic` makes that undeniable — it exists
    // precisely to reach another machine — so the row now states the weaker,
    // true thing. Both classes sit on the safe side of the only gate that
    // enforces (RemotePayload / QueryEgress must live in BOUNDARY_MODULE), so
    // nothing about the build changes; what changes is that a reviewer reading
    // this row is no longer told these clients stay on the box.
    //
    // Mesh is the right ceiling: `dynamic`'s resolver is `PeerEndpointSource`,
    // the mesh's own view, which can only ever name a peer of this node's
    // mesh — and an unresolvable binding is an `Err`, never a fallback to a
    // remembered address, so the site cannot reach a host the mesh has not
    // vouched for.
    ("oicp-client/src/lib.rs", Class::Mesh, 3),

    // ---- corpus-engine ----
    // testing.rs: the deterministic test-fixture module (never
    // modifies production indexes); acquirers + news stream:
    // InboundOnly downloads.
    ("corpus-engine/src/testing.rs", Class::TestOnly, 2),
    ("corpus-engine/src/update/newsworthy_event_stream.rs", Class::InboundOnly, 1),
    ("corpus-engine/src/acquirers/huggingface.rs", Class::InboundOnly, 1),
    ("corpus-engine/src/acquirers/http_api/mod.rs", Class::InboundOnly, 1),
    ("corpus-engine/src/acquirers/bulk_download.rs", Class::InboundOnly, 1),

    // ---- studio/sovereign-tools-base ----
    // orchestrator: constructions are `#[cfg(test)]` (TestOnly).
    // web/mod.rs: the search tool's default_client was removed with
    // the boundary move — hosts inject the boundary-built client
    // (sovereign-tools-base is contract-only and cannot reach
    // sovereign-core); the WebFetchTool site stays InboundOnly.
    ("studio/crates/sovereign-tools-base/src/web/search/orchestrator.rs", Class::TestOnly, 4),
    ("studio/crates/sovereign-tools-base/src/web/mod.rs", Class::InboundOnly, 1),
    ("studio/crates/sovereign-tools-base/src/mcp/http.rs", Class::OperatorSurface, 1),

    // ---- studio/sovereign-workflow-host (LocalDaemon) ----
    ("studio/crates/sovereign-workflow-host/src/installer.rs", Class::LocalDaemon, 2),
    ("studio/crates/sovereign-workflow-host/src/lib.rs", Class::LocalDaemon, 1),

    // ---- studio/sovereign-recipe-author ----
    ("studio/crates/sovereign-recipe-author/src/probe_url.rs", Class::InboundOnly, 1),
    ("studio/crates/sovereign-recipe-author/src/http_tester.rs", Class::LocalDaemon, 1),

    // ---- commonwealth (the estate's own web app + shards; Mesh / LocalDaemon) ----
    ("commonwealth/crates/commonwealth-knowledge/src/shard_manager.rs", Class::Mesh, 3),
    ("commonwealth/crates/commonwealth-knowledge/src/embed_http.rs", Class::LocalDaemon, 2),
    ("commonwealth/crates/commonwealth-api/src/routes_internal/corpus_collaborate.rs", Class::Mesh, 2),
    ("commonwealth/crates/commonwealth-api/src/routes_knowledge.rs", Class::Mesh, 1),
    ("commonwealth/crates/commonwealth-api/src/routes_internal/pipeline_pause.rs", Class::LocalDaemon, 1),
    ("commonwealth/crates/oicp-conformance/src/checks.rs", Class::LocalDaemon, 1),
    ("commonwealth/crates/commonwealth-app/src/proxy.rs", Class::LocalDaemon, 1),
];

// ---------------------------------------------------------------------------
// Instrument
// ---------------------------------------------------------------------------

/// The workspace root: the ancestor of CARGO_MANIFEST_DIR whose
/// Cargo.toml declares `[workspace]`.
fn workspace_root() -> PathBuf {
    let mut dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    loop {
        let toml = dir.join("Cargo.toml");
        if toml.is_file()
            && fs::read_to_string(&toml)
                .map(|t| t.contains("[workspace]"))
                .unwrap_or(false)
        {
            return dir;
        }
        if !dir.pop() {
            panic!(
                "workspace root not found from {}",
                env!("CARGO_MANIFEST_DIR")
            );
        }
    }
}

/// The workspace member paths from the root Cargo.toml `members`
/// list (deterministic scan scope — a new member auto-enters).
fn workspace_members(root: &Path) -> Vec<String> {
    let text = fs::read_to_string(root.join("Cargo.toml")).expect("root Cargo.toml");
    let start = text
        .find("members = [")
        .unwrap_or_else(|| panic!("no members list in root Cargo.toml"));
    let end = text[start..].find(']').expect("unterminated members list") + start;
    text[start + "members = [".len()..end]
        .lines()
        .map(|l| l.trim().trim_matches(',').trim_matches('"'))
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(String::from)
        .collect()
}

fn walk_rs(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(dir).expect("read_dir") {
        let p = entry.expect("dir entry").path();
        if p.is_dir() {
            walk_rs(&p, out);
        } else if p.extension().map(|e| e == "rs").unwrap_or(false) {
            out.push(p);
        }
    }
}

/// Every production .rs file (member `src/` trees only), sorted.
fn production_src_files(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for member in workspace_members(root) {
        let src = root.join(&member).join("src");
        if src.is_dir() {
            walk_rs(&src, &mut out);
        }
    }
    out.sort();
    out
}

/// Repo-relative path ("sovereign/crates/..." form — the registry's
/// key space).
fn rel(root: &Path, p: &Path) -> String {
    p.strip_prefix(root)
        .unwrap_or(p)
        .to_string_lossy()
        .replace('\\', "/")
}

/// Construction-site count for one file, with the same semantics as
/// the 2026-08-17 survey probe: a line counts if it contains a
/// `reqwest::(blocking::)?Client::(new|builder)(` token, or — in
/// files that `use reqwest` — a bare `Client::(new|builder)(`.
fn count_sites(text: &str) -> usize {
    let has_import = text.contains("use reqwest");
    const PREFIXED: [&str; 4] = [
        "reqwest::Client::new(",
        "reqwest::Client::builder(",
        "reqwest::blocking::Client::new(",
        "reqwest::blocking::Client::builder(",
    ];
    let mut n = 0;
    for line in text.lines() {
        if PREFIXED.iter().any(|p| line.contains(p)) {
            n += 1;
            continue;
        }
        if has_import && (line.contains("Client::new(") || line.contains("Client::builder(")) {
            n += 1;
        }
    }
    n
}

/// Scan all production src trees → (repo-relative path, site count).
fn scan(root: &Path) -> BTreeMap<String, usize> {
    let mut sites = BTreeMap::new();
    for p in production_src_files(root) {
        let text = fs::read_to_string(&p).expect("read source file");
        let n = count_sites(&text);
        if n > 0 {
            sites.insert(rel(root, &p), n);
        }
    }
    sites
}

// ---------------------------------------------------------------------------
// The gates
// ---------------------------------------------------------------------------

/// F26 — the egress-boundary census (bar dr-egress). Every
/// RemotePayload / QueryEgress construction must live in
/// BOUNDARY_MODULE; every detected site must be registered; every
/// registered site must still exist with the same count.
#[test]
fn f26_egress_boundary_census() {
    let root = workspace_root();
    let sites = scan(&root);
    let mut problems: Vec<String> = Vec::new();

    let mut registered: BTreeMap<&str, Class> = BTreeMap::new();
    for (path, class, count) in REGISTRY {
        registered.insert(path, *class);
        match sites.get(*path) {
            None => problems.push(format!(
                "STALE ROW: {path} — no construction sites found (registry says {count})"
            )),
            Some(&actual) if actual != *count => problems.push(format!(
                "COUNT DRIFT: {path} — registry {count}, census {actual} — a new construction site is a review moment"
            )),
            Some(_) => {}
        }
        if matches!(class, Class::RemotePayload | Class::QueryEgress) && *path != BOUNDARY_MODULE {
            problems.push(format!(
                "EGRESS OUTSIDE BOUNDARY: {path} ({}, {count} site(s)) — every remote-model / search-query client must be constructed in {BOUNDARY_MODULE}",
                class.as_str()
            ));
        }
    }

    match sites.get(BOUNDARY_MODULE) {
        Some(_) => {
            if registered.get(BOUNDARY_MODULE) != Some(&Class::Boundary) {
                problems.push(format!(
                    "{BOUNDARY_MODULE} exists but is not registered with class Boundary — register the row"
                ));
            }
        }
        None => problems.push(format!(
            "NO BOUNDARY MODULE: {BOUNDARY_MODULE} does not exist — the ONE egress choke point is not built"
        )),
    }

    for (path, count) in &sites {
        if !registered.contains_key(path.as_str()) {
            problems.push(format!(
                "UNREGISTERED: {path} ({count} site(s)) — classify it in the F26 registry"
            ));
        }
    }

    assert!(
        problems.is_empty(),
        "F26 egress-boundary census FAILED ({} problem(s)):\n{}",
        problems.len(),
        problems.join("\n")
    );
}

/// R-6 — one run-scoped fail-closed budget decider (bar
/// dr-budget-one-decider). No second decider: the identifiers of the
/// studio fail-open decider cluster (`budget_allows`,
/// `decrement_budget`, `BudgetView`) are absent from every
/// production src tree, and the ONE SpendDecider exists in
/// sovereign-core.
///
/// NOTE: `check_budget` is deliberately NOT forbidden — it names the
/// conversation-FRAME schema check in sovereign-tools
/// (frame.rs / conv_frame.rs / session_state.rs), a different
/// concept; the studio decider's own identifiers above cover it.
#[test]
fn r6_one_run_scoped_budget_decider() {
    let root = workspace_root();
    const FORBIDDEN: [&str; 3] = ["budget_allows", "decrement_budget", "BudgetView"];

    let mut hits: Vec<String> = Vec::new();
    for p in production_src_files(&root) {
        let text = fs::read_to_string(&p).expect("read source file");
        for (i, line) in text.lines().enumerate() {
            for id in FORBIDDEN {
                if line.contains(id) {
                    hits.push(format!("{}:{}: {id}", rel(&root, &p), i + 1));
                }
            }
        }
    }

    let decider_text = fs::read_to_string(root.join(SPEND_DECIDER_MODULE))
        .unwrap_or_else(|_| panic!("SpendDecider module missing: {SPEND_DECIDER_MODULE}"));
    assert!(
        decider_text.contains("SpendDecider"),
        "the ONE SpendDecider must exist in {SPEND_DECIDER_MODULE}"
    );

    assert!(
        hits.is_empty(),
        "R-6: a second budget decider is present ({} hit(s)):\n{}",
        hits.len(),
        hits.join("\n")
    );

    // Path-scoped (pre-registered, order deep-research-t2a Instrument
    // 3): the studio search tool's web_search path must be free of
    // ANY budget-decider identifier — including `check_budget`, which
    // the global scan cannot forbid because sovereign-core's
    // conversation-FRAME check is a different concept. The decider
    // move removed the search-path budget gate; a resurrected one
    // here is the exact R-6 shape.
    let search_rs_path = "studio/crates/sovereign-tools-base/src/search.rs";
    let search_rs = fs::read_to_string(root.join(search_rs_path))
        .unwrap_or_else(|_| panic!("{search_rs_path} missing"));
    for id in [
        "budget_allows",
        "decrement_budget",
        "BudgetView",
        "check_budget",
    ] {
        if search_rs.contains(id) {
            panic!(
                "R-6: {search_rs_path} contains a budget-decider identifier ({id}) — \
                 spend is gated once by the SpendDecider in sovereign-core"
            );
        }
    }
}
