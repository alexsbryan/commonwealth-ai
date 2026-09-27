// SPDX-License-Identifier: AGPL-3.0-or-later
//! The F26 census registry's second half (from the CLI crates on), split
//! so neither file enters the size band; `REGISTRY` and `REGISTRY_TAIL`
//! are read as one list by the census.

use super::Class;

#[rustfmt::skip]
pub(super) const REGISTRY_TAIL: &[(&str, Class, usize)] = &[
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
    ("sovereign/crates/sovereign-cli-mesh/src/ring_cmd/mod.rs", Class::LocalDaemon, 1),
    // pb-shell (42a657102, da819e9e2): a `#[test]` that builds a RingCtx it
    // never sends through, and the shell's own test module.
    ("sovereign/crates/sovereign-cli-mesh/src/ring_cmd/show.rs", Class::TestOnly, 1),
    ("host-kit/src/shell/tests.rs", Class::TestOnly, 1),
    // 2 -> 3 on 2026-08-21 (nc-27): `daemon_get` MOVED here from
    // `project_cmd/registry_watch.rs` when that file was deleted as an
    // unreachable fork. Same loopback client, same class — a relocation,
    // not a new egress site.
    ("sovereign/crates/sovereign-cli-dev/src/project_cmd/mod.rs", Class::LocalDaemon, 3),
    ("sovereign/crates/sovereign-cli-dev/src/code_map.rs", Class::LocalDaemon, 2),
    ("sovereign/crates/sovereign-cli-dev/src/drift_cmd_orchestrator.rs", Class::LocalDaemon, 1),
    // refactor_cmd/label_model: the name-group adjudication pass. One client,
    // pinned to the local daemon — it posts Rust source snippets and the
    // code-intel descriptions of the types under judgement, which are estate
    // content, so LocalDaemon is the class that keeps them on the machine. A
    // future `--daemon-url` pointing off-box would be the review moment, not
    // a count change.
    ("sovereign/crates/sovereign-cli-dev/src/refactor_cmd/label_model.rs", Class::LocalDaemon, 1),
    ("sovereign/crates/sovereign-cli-dev/src/code_capability_graph.rs", Class::LocalDaemon, 1),
    // +2 (2026-09-23, five-programs fp-32/fp-33): the workbench's two DIAL
    // clients — the state store and the mesh KV are the daemon's, so
    // `audit --recover` and the work-atlas surfaces read them over the
    // daemon's own /v1 routes, one client construction each. Estate content
    // never leaves the machine: LocalDaemon, like every row above.
    // -1 (pb-atlas-kv): the mesh-KV twin is gone; the work atlas dials
    // through turn-client's `rails_kv.rs`, counted there.
    ("sovereign/crates/sovereign-cli-dev/src/state_store_client.rs", Class::LocalDaemon, 1),

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
    // NEW (fp-cond2-c, a1aaa0463): the terminal-join tests spawn a founder
    // daemon and poll it, then hand `find_holders` a probe client — both
    // loopback to the child the test itself started. Test fixtures.
    (
        "sovereign/crates/sovereign-cli-daemon/src/setup_cmd/terminal/join_child/tests.rs",
        Class::TestOnly,
        2,
    ),

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

    // rail.rs (2026-09-09): the one rail append/read client moved out of
    // sovereign-cli-llm into the crate every CLI links (ded2e10b0), so that
    // `svrn quality check --distribute` could submit a handoff without linking
    // the LLM dispatcher. CLASSIFIED FRESH for the same reason `code_index`
    // above was — a client constructor moving from a leaf binary into a SHARED
    // library is a different reachability story on its face. The three checks:
    //   - Destination is the operator's own daemon, and unlike `code_index` it
    //     is NOT a pinned localhost literal. `urls::daemon_base_url` delegates
    //     to `setup_config::client_daemon_base`, which honours
    //     `SOVEREIGN_DAEMON_URL` and `[daemon] client_port` — so an operator
    //     who aims that knob at another host sends these bytes there. That is
    //     the knob's declared purpose and is true of every `daemon_base_url`
    //     caller already in this registry, but it is named here rather than
    //     glossed: a reader checking this row against "never leaves the
    //     machine" deserves the exception in front of them.
    //   - What travels is the operator's own signed rail acts — a submission,
    //     an offer, a report. No corpus content is read out and sent, and the
    //     receiving daemon refuses any act whose signer its roster does not
    //     carry.
    //   - Reachability widened ON PURPOSE. Before ded2e10b0 only
    //     sovereign-cli-llm could construct this; now every binary linking
    //     sovereign-cli-shared can. That widening IS the refactor, and the
    //     class is unchanged by it because the destination did not move.
    //   - fp-98 (9e6c557c0) moved the file by git mv into the sovereign-cli-base
    //     leaf behind its `rail-client` feature; same client, same destination.
    ("sovereign/crates/sovereign-cli-base/src/rail.rs", Class::LocalDaemon, 1),

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
    ("corpus-engine-notes/src/mining/diff_extract_backend.rs", Class::LocalDaemon, 1),
    ("sovereign/crates/sovereign-tools/src/local_corpus/ocr/cleanup.rs", Class::LocalDaemon, 1),
    ("sovereign/crates/sovereign-tools/src/corpus/manager.rs", Class::InboundOnly, 1),
    ("sovereign/crates/sovereign-tools/src/catalog_ingest.rs", Class::LocalDaemon, 1),
    ("sovereign/crates/sovereign-tools/src/calendar.rs", Class::OperatorSurface, 1),


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

    // ---- sovereign-tdd (LocalDaemon — TDD loop against the daemon) ----
    ("sovereign/crates/sovereign-tdd/src/backend.rs", Class::LocalDaemon, 2),
    ("sovereign/crates/sovereign-tdd/src/recur/model.rs", Class::LocalDaemon, 1),

    // ---- sovereign-compute ----
    // client: loopback back to the host daemon; supervisor: heartbeat
    // to compute pods (estate infrastructure, own auth).
    ("sovereign/crates/sovereign-compute/src/client.rs", Class::LocalDaemon, 1),
    ("sovereign/crates/sovereign-compute/src/supervisor.rs", Class::Mesh, 1),
    // NEW ROW (2026-09-26, phase-b pb-serve-program d2f6e781c): the one kind
    // mount's test posts to a child router it bound on 127.0.0.1:0.
    ("sovereign/crates/sovereign-compute/src/server.rs", Class::TestOnly, 1),
    // NEW ROW (2026-09-27, REVIEW-audit-pb-auto-4): the NER client posts to
    // serve's /v1/ner at the base the daemon dials (53153e1b1) — serve's
    // loopback port on this host.
    ("sovereign/crates/sovereign-compute/src/ner.rs", Class::LocalDaemon, 1),

    // ---- serve, dialed by the svrn daemon (pb-svrn-dials-serve) ----
    // serve_client: the engine-state, served-self, forwarded-GET and reload
    // reads, each to serve's loopback base (venue::DEFAULT_SERVE_PORT or
    // SOVEREIGN_SERVE_PORT).
    ("sovereign/crates/sovereign-daemon/src/serve_client.rs", Class::LocalDaemon, 4),
    // fetch-model's peer client moved here, whole, from sovereign-cli-mesh's
    // mesh_cmd.rs (c2529c94c): the mesh row went 8 -> 7, same class.
    ("sovereign/crates/sovereign-serve/src/fetch_model.rs", Class::Mesh, 1),
    // lib.rs and reload.rs: `#[cfg(test)]` modules posting to a router the
    // test bound on loopback.
    ("sovereign/crates/sovereign-serve/src/lib.rs", Class::TestOnly, 2),
    ("sovereign/crates/sovereign-serve/src/reload.rs", Class::TestOnly, 2),

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
    // Mesh is the right ceiling: `dynamic`'s resolver is `VenueSource`,
    // the mesh's own view, which can only ever name a peer of this node's
    // mesh — and an unresolvable binding is an `Err`, never a fallback to a
    // remembered address, so the site cannot reach a host the mesh has not
    // vouched for.
    ("oicp-client/src/lib.rs", Class::Mesh, 3),

    // ---- corpus-engine ----
    // testing.rs: the deterministic test-fixture module (never
    // modifies production indexes); acquirers: InboundOnly
    // downloads. The newsworthy EventStreams subscriber
    // (`update/newsworthy_event_stream.rs`) was the fourth row here
    // until cw-lift 2b deleted it — its `EventStreamHost` trait had
    // no implementor in the workspace, so the SSE loop never ran.
    ("corpus-engine/src/testing.rs", Class::TestOnly, 2),
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
    // The embed-model resolution that lived here and in
    // `sovereign-cli-llm/src/recipe_cmd.rs` (one site each) became ONE
    // decider in `daemon_models.rs` on 2026-09-01 (issue #57: the listing
    // check refused a daemon that could embed). Both sites moved with it —
    // same class, same total, one path instead of two. The second site is
    // the /v1/embeddings probe that replaced the id-substring test: it
    // asks the LOCAL daemon to embed a short fixed string, so nothing
    // leaves the machine and no estate content is in the payload.
    ("studio/crates/sovereign-workflow-host/src/daemon_models.rs", Class::LocalDaemon, 2),
    // NEW (2026-09-09, sv-surface rung 5): the daemon's /internal/workflows/*
    // job surface lives here, and its router tests are the only client
    // constructions — the end-to-end job test and the loopback-guard
    // fails-closed test, both inline `#[cfg(test)]` against a spawned
    // loopback listener. Production traffic is the DAEMON's own in-process
    // provider; no client, no egress.
    ("studio/crates/sovereign-workflow-host/src/workflow_http.rs", Class::TestOnly, 2),

    // ---- studio/sovereign-recipe-author ----
    ("studio/crates/sovereign-recipe-author/src/probe_url.rs", Class::InboundOnly, 1),
    ("studio/crates/sovereign-recipe-author/src/http_tester.rs", Class::LocalDaemon, 1),

    // ---- commonwealth (the estate's own web app + shards; Mesh / LocalDaemon) ----
    ("sovereign/crates/sovereign-grants/src/shard_manager.rs", Class::Mesh, 3),
    // `http_embed_fn` moved DOWN to corpus-engine 2026-09-03 (enrichment-as-
    // plugin Step 5). What was left behind — the `/v1/models` reconstruction
    // probe, `embed_model_info` — had ZERO callers, so the file went with the
    // rest of the dead-twin sweep the same day and its row went with it.
    // The `POST /v1/embeddings` EmbedFn constructor. The URL is the CALLER's:
    // the daemon's own endpoint from commonwealth-knowledge, or the operator's
    // `--base-url` from corpus-mcp — an operator-owned target, so it carries
    // the operator's class, never RemotePayload's exemption.
    ("corpus-engine/src/embed_http.rs", Class::OperatorSurface, 1),
    // corpus-mcp's ONE client constructor (`host::client()`) and every probe
    // that rides it: `GET /oicp/v1/capabilities`, `GET /v1/models`, one `POST
    // /v1/embeddings`, `corpus ingest`'s chat probe, and the endpoint
    // discovery ladder — all against the operator's own endpoint, whether
    // named with `--base-url` or found on the ladder (Ollama :11434,
    // llama-server :8080, this host's OICP daemon).
    //
    // 2 -> 1 at order ei-6-distribution: `corpus ingest` built its own
    // `reqwest::Client::new()` for the chat capability probe, a second answer
    // to "how long do we wait on an endpoint" beside the embed probe's — and
    // the discovery ladder made the absent timeout load-bearing (three
    // unreachable rungs at reqwest's default is an unbounded hang). One
    // constructor with one timeout, ARCH §10.6.
    //
    // The HF pull `corpus serve` performs when a named corpus is absent adds
    // NO row: it goes through `CorpusEngine::ingest` to
    // corpus-engine/src/acquirers/bulk_download.rs, registered InboundOnly
    // above since this census was written.
    ("corpus-mcp/src/host.rs", Class::OperatorSurface, 1),
    // 2 -> 1 at cw-lift rung 2c: the queue-handoff unicast to
    // `/internal/app/state` built its own client with its own 10s timeout,
    // a second answer to "how long do we wait on a peer" beside
    // `gossip_client()`. The gossip round already replicated the row.
    // Re-keyed 2026-09-18 (REVIEW-audit-daemon-2): the api host cluster moved
    // to `sovereign-daemon` at `dm-daemon-api-edge`. Path only — the three
    // sites and their classes travelled with their files.
    ("sovereign/crates/sovereign-daemon/src/routes_internal/corpus_collaborate.rs", Class::Mesh, 1),
    ("sovereign/crates/sovereign-daemon/src/routes_knowledge.rs", Class::Mesh, 1),
    ("sovereign/crates/sovereign-daemon/src/routes_internal/pipeline_pause.rs", Class::LocalDaemon, 1),
    // NEW (2026-09-18, ring-doc REVIEW-build-rd-1-live, 6ac1fd39f; re-keyed to
    // sovereign-daemon with the api host cluster): the ring live lane's fan-out.
    // `push_ephemeral` POSTs a namespaced envelope to `/internal/ring/live` on each
    // Online mesh member through the `PeerTransport` seam — ring peers, never a third party.
    ("sovereign/crates/sovereign-daemon/src/routes_rail_live.rs", Class::Mesh, 1),
    // NEW ROW (2026-09-19, ring-room rr-2-media-posture c24fe521a): the
    // holder's media-presence poll. RECOUNTED (2026-09-24, fp-47's dial,
    // 884ed301c): the origin ask moved to the rails daemon — this file
    // reads `GET /v1/mesh/media/presence` through `rails_client`'s ONE
    // shared client — leaving ONE site here, the self-report to
    // `{internal_url}/internal/node/activity`, this process reporting to
    // itself, loopback whatever `internal_bind` says (`daemon.rs:3383-3385`),
    // still `LocalDaemon` for that half. The operator-configured media origin
    // is no longer asked by THIS file at all; the poll's own row is
    // `commonwealth-rails/src/presence.rs` below.
    (
        "sovereign/crates/sovereign-daemon/src/media_presence.rs",
        Class::LocalDaemon,
        1,
    ),
    // NEW ROW (2026-09-24, fp-54's flip): the shared dial client for every
    // serving-process verb — roster-names, presence, forget-member, and the
    // ring rail's port (`RailsRingRail`). One construction, one destination:
    // the loopback `rails_base` (default 127.0.0.1:9747), the mesh's rails
    // daemon this daemon already trusts with its roster answers since fp-6.
    ("sovereign/crates/sovereign-daemon/src/rails_client.rs", Class::LocalDaemon, 1),
    // NEW ROW (2026-09-25, fp-solo-hermetic, five-programs-66): a local-only
    // node's `ensure_rails` reads an already-running cw-rails'
    // `GET /v1/mesh/status` posture — the same loopback `rails_base`, which
    // `ensure_rails` refuses unless it is loopback. Its own client because
    // the read runs on ensure_rails' short-lived runtime, not the daemon's.
    // Moved with `ensure_rails` to `svrn mesh up` (pb-rails-untether).
    (
        "sovereign/crates/sovereign-cli-mesh/src/rails_up.rs",
        Class::LocalDaemon,
        1,
    ),
    // NEW ROW (2026-09-24, fw-1 wave: the journals moved to the rails
    // daemon, so the presence poll moved with the media origin it reads).
    // Four sites, one of them production — `run`'s poll client — dialing
    // `[media] origin` from rails.toml, and `OperatorSurface` rather than
    // `LocalDaemon` is the honest class for it: the address the operator
    // put in the config is the address asked — usually the Jellyfin on this
    // machine, not necessarily. It carries the HOUSE credential and asks
    // `GET /Sessions`; no estate content goes out and no third-party host is
    // reachable from here. The other three sites are the inline test
    // module's fixtures against an origin it spawns itself.
    (
        "commonwealth/crates/commonwealth-rails/src/presence.rs",
        Class::OperatorSurface,
        4,
    ),
    ("oicp-conformance/src/checks.rs", Class::LocalDaemon, 1),
    ("sovereign/crates/sovereign-meshapp-registry/src/proxy.rs", Class::LocalDaemon, 1),
    // Federated media's catalogue half. The row was
    // `sovereign-mesh/src/media_fanout.rs` from 2026-09-11 until the decisions
    // moved to the package crate later the same day (0eccf5664) — same two
    // sites, same class, one fewer daemon that can answer the question
    // differently. One client per fan-out, driven through each member's
    // loopback bridge to that member's media origin over the estate's own
    // transport; the second site is the unit test's local origin.
    // 2 -> 4 (2026-09-12): the two new `#[cfg(test)]` origins that prove the
    // row's `json` field — one JSON answer parsed, one truncated answer
    // deliberately not. Test fixtures in an inline module, same as
    // `mesh_skew.rs`'s three; the production sites are still the two above.
    ("commonwealth/crates/commonwealth-media/src/fanout.rs", Class::Mesh, 4),

    // ---- commonwealth-rails: the package-only rails daemon (2026-09-11) ----
    // Every destination is a peer of the operator's own mesh, reached through
    // a loopback bridge this process minted, over a QUIC connection dialed by
    // the peer's Ed25519 key. Nothing here has a third-party endpoint to
    // point at: the URL is always `127.0.0.1:<ephemeral>` and the far end is
    // a member.
    ("commonwealth/crates/commonwealth-rails/src/gossip.rs", Class::Mesh, 1),
    ("commonwealth/crates/commonwealth-rails/src/join.rs", Class::Mesh, 1),
    // `cw-rails media` is a client of THIS daemon's own loopback API — the
    // same bytes a `curl` would send, and they never leave the machine.
    ("commonwealth/crates/commonwealth-rails/src/cli.rs", Class::LocalDaemon, 1),
    // The `#[cfg(test)]` modules of the kv doors (fp-77, fp-107, fp-109) and the
    // ledger doors (fp-78): each client talks to a router the test itself served
    // on loopback.
    ("commonwealth/crates/commonwealth-rails/src/kv/tests.rs", Class::TestOnly, 4),
    ("commonwealth/crates/commonwealth-rails/src/ledger/tests.rs", Class::TestOnly, 1),
];
