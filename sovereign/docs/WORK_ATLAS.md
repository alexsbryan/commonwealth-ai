# Work Atlas — Coordination for Agents on a Shared Mesh

## Why this exists

Agents — Claude instances, human developers, scripted bots — increasingly share a repo on the same Commonwealth mesh. Today there's no shared surface that says "someone is touching `CorpusEngine::ingest()` right now." Collisions surface late: at merge time, after duplicated work, or in a stuck PR review. The Work Atlas gives the daemon a normative coordination layer that other agents can query before they start.

**Phase 1** shipped the *explicit-coordination half* — `declare_scope` / `release_scope` / `work_in_flight` MCP tools and the `sovereign claim` CLI.

**Phase 2** (this branch) ships the *passive-observation half*. There is **no new CLI**. The daemon's CodeWatcher already fires on every edit; an `AtlasObserver` plugged into the existing `WatcherCoordinator` turns those events into `ObservationRecord`s and gossips them to peers within one HTTP round-trip. Agents calling `work_in_flight` see live edits from any peer on the mesh, graded `Active` (≤5 min since last edit) or `Recent` (≤30 min).

## Cross-mesh demo — run this end-to-end

The whole point of Phase 2 is that this works without anyone declaring or claiming anything.

**On both mac-peer and linux-peer:**
```
sovereign daemon start
```
(Wait for `sovereign daemon: work-atlas observer wired on <repo>` and `work_atlas: real broadcaster wired (peer fan-out active)` in the logs.)

**On mac-peer:** edit any file in the repo — e.g. touch a comment in `corpus-engine/src/engine/ingest.rs`.

**On linux-peer**, immediately after:
```
sovereign tools call work_in_flight \
  --scope=corpus-engine/src/engine/ingest.rs \
  --match_mode=file
```
Expected output: a `claims: []` array and an `observations: [...]` array with one entry, `node_id` = mac-peer's, `confidence` = `active`. Within 30 s of the last edit it's still `active`; within 30 min it drops to `recent`; after that, the observation is no longer surfaced (the record may persist briefly before GC sweeps it).

If the observation doesn't show: check that both daemons are in the same mesh (`sovereign mesh status` lists peers), and that both repos resolve a `repo_id` (`git config --get remote.origin.url` returns a non-empty value — Phase 1's MUST gate is upheld here too, the observer is a no-op without it).

## Model in one paragraph

Four entities: **Node** (mesh `node_id`, reused — no parallel identity scheme), **Session** (one per (agent, agent_session_token, repo_id) triple — a Claude instance or a human at a workstation), **Claim** (explicit declared intent on a symbol or path, TTL-bounded), **Observation** (passive activity from CodeWatcher — Phase 2). Every Claim and Observation attributes to exactly one Session. Storage is MeshStore under `app_id = "work-atlas"` (or `"work-atlas-private"`). Records expire on TTL — there is no history surface; the spec treats this as a point-in-time invariant.

## Phase 1 tradeoffs

- **Point-in-time, not historical.** A released claim is *gone*. No "show me everyone who has ever touched this function" surface — that's git's job. Spec §3 is explicit: dropping a Claim is not an event the atlas records.
- **Declared grade only.** `work_in_flight` Phase 1 returns only claims explicitly declared via the tool / CLI. The `Active` / `Recent` / `Exploring` grades require CodeWatcher-driven Observations (Phase 2).
- **No git co-evolution.** `sovereign claim check <scope>` emits a Phase-2-deferred footer rather than calling `corpus-engine::git_archaeology::compute_co_evolution` today.
- **CLI is one-Session-per-workstation.** `sovereign claim …` synthesizes a `cli:<node>` agent_session_token. Phase 2's CodeWatcher idle-gap synthesis (30-min cold start, 4h idle drop) supersedes this with multiple Sessions correlated to active editing windows.

## Privacy — three structural layers (ARCH §7.4)

Spec §6 mandates: "A Private Session MUST result in zero MeshStore records for that Session, its Claims, or its Observations replicated." This is a user-trust invariant, so three layers each independently enforce it:

1. **Store level (structural).** `Privacy::app_id()` is a `const fn` returning one of two hardcoded literals — `"work-atlas"` or `"work-atlas-private"`. No code path constructs an app_id from runtime data. The violation is impossible to *express*. Pinned by `privacy_app_id_returns_hardcoded_literals` in `model.rs`.
2. **Gossip level (structural).** `"work-atlas-private"` is in `GOSSIP_EXCLUDED_APP_IDS` in `commonwealth-state::peer_preferences`. The gossip loop's `all_entries_for_gossip` filters at the slice; a private record cannot reach the network even if a writer asks. Pinned by `gossip_excludes_work_atlas_private_app_id`.
3. **Read level (defence-in-depth).** `work_in_flight` filters its results to Public records regardless of whether the caller has visibility into Private ones. `broadcast_now` in `sovereign-mesh::gossip` also rejects calls with a private app_id, logging at WARN — a sloppy caller cannot trigger a leak.

Toggling Public ↔ Private does not retroactively republish prior records. The toggle starts a new Session; the old Session keeps its original app_id until TTL drops it (spec §6).

## Identity decisions

- **Agent identity via header.** `X-Agent-Session` is extracted in `mcp_router::mcp_handle` and threaded into `ToolContext::agent_session_token`. Anonymous calls (no header) fall back to `format!("conn:{mcp_session_id}")` so per-connection grouping still works.
- **`repo_id` is MUST.** Per spec §10 (escalated from SHOULD on the user's call) the daemon refuses to create Sessions outside a git repo with an `origin` remote. `repo_id` is SHA-256(canonicalized origin URL) — SSH and HTTPS forms of the same repo hash to the same value, so two workstations clones agree.
- **`node_id` is reused, not duplicated.** Spec §1 — atlas identity equals mesh identity. Stable per workstation via `~/.svrnmesh/node_id`.
- **`node_is_self` says whether a record is yours** (added 2026-08-07). Every claim and observation carries it alongside `node_id`, computed in `collect_in_flight` against `WorkAtlasStore::node_id()`, so both consumers — the `work_in_flight` tool and the session-boot briefing — agree. It exists because **scope strings are not node-qualified**: a host-local resource gets the same name on every workstation, so `daemon-runtime:9741-primary-slot` is one bucket holding every node's claim on its *own* daemon. `node_id` alone cannot resolve that — it is an opaque hash and nothing else in the response says which one is the caller's — so a peer's claim reads as a lock on the box you are sitting on. That misread cost real stalled work on 2026-08-07. Pinned by `tests/cross_node.rs::same_scope_on_two_nodes_is_distinguishable_by_node_is_self`. Node *names* (`BeefyMac`, `RuggedFox`) are **not** available here — they live in the mesh roster (`svrn mesh status`, which marks self with `*`), and the work-atlas store holds only `NodeId`.

## TTL model

- Claims have a per-record `ttl_expires_at` (default 4h, max 24h, both configurable in `~/.svrnmesh/work-atlas.toml`). MeshStore's built-in `gc(ttl_seconds)` is app-wide and entry-timestamp-based; the atlas runs its own `WorkAtlasGc` on a 60s sweep that reads each `ClaimRecord`'s `ttl_expires_at` directly.
- **Eviction writes an abandonment tombstone** (order `commons-fluency`
  fix 2): when a Public claim's TTL expires, the sweep writes a
  `ClaimTombstone` (Public namespace, key `claim-tombstone:<id>`,
  carrying `{claim_id, session_id, node_id, intent, symbol_refs,
  ttl_expires_at, evicted_at}`) before dropping the claim, retained
  `EXPIRED_TOMBSTONE_TTL_SECS` (1h). This is what keeps `resource_may_i`'s
  `expired` verdict readable past the sweep — see the resource-commons
  section. Private evictions write no tombstone (private never gossips,
  so abandonment evidence would be local-only and misleading). The
  idle-session cascade does not tombstone either: that path drops a
  whole session, and the point-in-time invariant says its records
  disappear.
- Sessions drop after `idle_timeout_seconds` (default 4h) since `last_activity_at`. Cascade: remaining claims attributed to the session are released too.
- `work_in_flight` also gates on TTL at read time so a claim past its expiry but not yet swept doesn't appear.

## Resource claims — the shared-resource commons (order `seat-resource-commons`)

The same claims rail carries a *resource* convention: a seat asks "is
this shared resource serving someone else right now?" and says "I am
taking it" in a way other seats can see. This is Ostrom's commons
frame — visibility and graduated response, **not** a lock manager: a
verdict never blocks, and a seat may always override with its reason
recorded.

- **Scope naming.** `daemon:<node>:<action>`, e.g. `daemon:BeefyMac:restart`.
  The node is the mesh roster's display name, which makes the
  documented host-local collision (every node's daemon on :9741) a
  non-issue: `daemon:RuggedFox:restart` and `daemon:BeefyMac:restart`
  are different buckets by construction. (File/symbol claims still
  need `node_is_self`, which is why that field exists — see Identity
  decisions.)
- **Query is EXACT match.** `resource_may_i` uses `ScopeMatch::Symbol`
  semantics, so `daemon:BeefyMac:restart` does not answer for
  `daemon:BeefyMac:restart-verify`. "Is THIS resource taken?" is an
  equality question.
- **TTL default is 30 minutes** (`DEFAULT_RESOURCE_TTL_SECS` in
  `sovereign-work-atlas/src/tools/resource_may_i.rs`), supplied by
  `sovereign claim take` and clamped by the daemon's `declare_scope`
  against the configured max. Shorter than the general claim default
  (4h) on purpose: a resource claim answers "is someone mid-operation
  on this right now?", and a mid-operation window of minutes, not
  hours, is the honest answer.
- **Expired ≠ released.** `work_in_flight` TTL-filters at read time,
  so an abandoned claim is invisible and "did the peer die?" is
  unanswerable. `resource_may_i` scans including expired rows and
  reports three verdicts:
  - `held` — a live claim names the scope, with node attribution,
    intent, and seconds remaining;
  - `expired` — claims exist but every one outlived its TTL: someone
    STARTED and never released, so the work may have died mid-run
    (the order's negative control; a real reaper incident on
    2026-08-11 is exactly this shape);
  - `free` — never claimed, or explicitly released (the work
    finished). Those two mean different things, and `expired` keeps
    them distinguishable.
- **`expired` outlives the sweep.** A TTL-expired claim row is swept
  within ≤60s of its expiry, but the eviction tombstone (above) keeps
  the `expired` verdict readable for 1h, with `abandoned_seconds_ago`
  measured from the claim's TTL (when the taker's work stopped being
  live) and `evicted_at` carried for GC bookkeeping. Before this
  (drill 2026-08-12, defect 1), `expired` collapsed into `free` within
  one sweep, making the UC-R3 negative control unobservable.
- **Attribution rides the claim** (order `commons-fluency` fix 1):
  `ClaimRecord.node_id` is embedded at declare time, and
  `resource_may_i` / `work_in_flight` read it from the claim — the
  one canonical carrier — never from the session record. (Before this,
  a peer resolved the node through the session row, which replicates
  slower than the claim: "held, by whom-unknown" for 1-4 min after a
  take. Claims written by an older binary lack the field; readers
  fall back to session resolution, named in a debug trace.)

## Broadcast model

Spec §7 calls for immediate fan-out on Claim writes (don't make a peer wait 10s for the next gossip round). `sovereign-mesh::gossip::broadcast_now(app_id, key)` reads the single entry and POSTs to every online peer's `/internal/app/state`, in parallel, fire-and-forget. Best-effort: unreachable peers are skipped with a WARN — the next gossip round will pick it up via the normal anti-entropy path.

Session updates do NOT trigger immediate fan-out (they're high-volume and the peers don't act on them); Observation writes (Phase 2) also won't.

## Observability (ARCH §9)

| Event | Level | Site |
|---|---|---|
| `work_atlas:claim_declared` | info | `DeclareScopeTool::execute` after store write |
| `work_atlas:claim_released` | info | `ReleaseScopeTool::execute` after delete |
| `work_atlas:claim_evicted_ttl` | info | `WorkAtlasGc::sweep_once` (tombstone written first for Public) |
| `work_atlas:tombstone_purged` | debug | `WorkAtlasGc::sweep_once` retention pass |
| `work_atlas:session_evicted_idle` | info | `WorkAtlasGc::sweep_once` |
| `work_atlas:query` | debug | `WorkInFlightTool::execute` |
| `work_atlas:resource_may_i` | debug | `ResourceMayITool::execute` (scope, verdict, node) |
| `work_atlas:claim_node_fallback_session` | debug | node attribution fell back to the session row (claim written by an older binary) |
| `work_atlas:broadcast_now_failed` | warn | per-peer failure in `broadcast_now` |
| `work_atlas:repo_id_missing` | warn | `repo_id::resolve` error path; daemon serve continues but `declare_scope` rejects |
| `mcp:tool_call dispatched` | debug | `mcp_router::handle_tool_call`, includes redacted token |

`agent_session_token` is truncated to 12 chars in logs (ARCH §9.3).

## Surfaces

### CLI

```
sovereign claim <symbol-or-path> --intent <text> [--ttl <seconds>]
sovereign claim check <symbol-or-path>
sovereign claim list [--mine | --all]
sovereign claim release <claim-id>
sovereign claim may-i <resource-scope>
sovereign claim take <resource-scope> --intent <text> [--ttl <seconds>]
```

**DAEMON-FIRST:** every verb calls the daemon's MCP tools when the
daemon answers, because the daemon's store is the one peers, gossip,
and CodeWatcher observations share. The in-process repo-local
`.sovereign/mesh.db` is a FALLBACK for daemon-down operation only,
and says so loudly — a claim written there is invisible to every
other process. `may-i`'s verdict depends on the daemon's store in
particular: the local fallback cannot see peer claims, so a daemon
running a build without `resource_may_i` reports `daemon rejected
resource_may_i` rather than silently answering from a store nobody
reads (§18.3). A sandboxed daemon is reached via the established
`SOVEREIGN_DAEMON_URL` knob (the sandbox lane points it at its
isolated port) — daemon-first calls resolve their target there, one
accessor per path (§10.6, `sovereign_cli_shared::urls::daemon_base_url`).
`--format json` mirrors `sovereign tools`. The CLI
uses `cli:<node_id>` as the synthetic agent_session_token, so two
`sovereign claim` invocations from the same workstation share a
session.

### MCP

Registered in `MCP_TOOLS_ALWAYS`:

| Tool | Effect | Idempotency | Latency | Scope |
|---|---|---|---|---|
| `declare_scope` | Write | NonIdempotent | Fast | Persistent |
| `release_scope` | Write | Idempotent | Fast | Persistent |
| `work_in_flight` | Read | Idempotent | Fast | Persistent |
| `resource_may_i` | Read | Idempotent | Fast | Persistent |

`resource_may_i` is the resource-commons read surface (see the
section above): one call, three verdicts, exact scope match, never
blocks.

The existing `blast` tool gains a `concurrent: [{ claim_id, session_id, intent, node_id }]` field. Present-but-possibly-empty per spec §8; emitted on every response regardless of atlas state.

## How Phase 2 works under the hood

`AtlasObserver` implements `corpus_engine::update::watcher_coordinator::BackgroundWatcher`. The daemon's existing `WatcherCoordinator` (the one that already drives the lint and test watchers) registers it alongside those — same notify watcher, same 800 ms debounce flush, same fan-out across all registered watchers. When the coordinator calls `on_files_changed(paths)` on the observer, it:

1. Ensures an ambient `SessionRecord` exists for `(node_id, repo_id)` — one per workstation+repo. The session's `agent_session_token` is `edits:<node>:<repo_short>`, intentionally distinct from the CLI's `cli:<node>` so the explicit-vs-passive distinction stays legible.
2. Applies its own 30 s per-path debounce — the spec's minimum interval between observation upserts. The coordinator's 800 ms debounce coalesces editor save-storms; the observer's 30 s on top stabilises the signal peers see.
3. For each non-debounced path: reads any existing `ObservationRecord` (preserves `first_observed_at`, bumps `event_count`), writes the updated record under `work-atlas:observation:<session_id>:<path>` (or `work-atlas-private` when the configured privacy is Private), and calls `broadcaster.broadcast(...)` for Public records. Paths are normalized to **repo-relative** at write time (as are `declare_scope` file scopes) — the canonical shape every `work_in_flight --match_mode=file` query should use. An empty scope in file mode matches everything (the supported "all live signals" query).

The broadcaster is a `DeferredBroadcaster` at observer construction time — the daemon's `AppState` isn't reachable when the watcher coordinator starts. A spawned task in `start_daemon` polls `daemon.app_state()` for up to 30 s and swaps the real `MeshBroadcaster` in once available. Until then, observations still propagate via the regular 10 s gossip round (just slower).

`work_in_flight` queries claims and observations independently, applies confidence grades (claims always `declared`; observations graded `active` / `recent` from `now - last_observed_at` against the spec windows), excludes the caller's own session, and returns both arrays.

## Phase 2 — confidence grades

| Grade | Source | Condition |
|---|---|---|
| `declared` | Claim | TTL not expired |
| `active` | CodeWatcher edit | `now - last_observed_at ≤ 300 s` (5 min) |
| `recent` | CodeWatcher edit | `now - last_observed_at ≤ 1800 s` (30 min) |
| `exploring` | Tool-call inspect (Phase 2b) | `now - last_observed_at ≤ 1800 s` |

Grades fall through automatically at read time: an observation that was `active` at 4 min becomes `recent` at 6 min, then drops from the result set at 31 min. No writer involvement needed.

## Phase 2b (deferred — explicit non-goals)

These are intentionally out of scope and called out so future-you doesn't think they were forgotten:

- **`.git/HEAD` ref-change monitoring.** Sessions stamp `current_branch` at creation but don't update on `git checkout`. Cleanest hook: extend the observer's `interesting_paths` filter to catch `.git/HEAD` and re-resolve.
- **Tool-call inspection observations** (`Exploring` grade). The wire-format already supports them (`ObservationSource::ToolCallInspect`, `ConfidenceGrade::Exploring`); the writer hook would live in `mcp_router::handle_tool_call` post-response, scoped to `callers` / `callees` / `symbols` / `blast` only.
- **Session segmentation by 30-min idle gap.** Phase 2 uses one ambient `edits:<node>:<repo>` session per workstation+repo, ever. The spec calls for new sessions after a 30-min idle gap; the cleanest implementation rotates the token to `edits:<node>:<repo>:<bucket>` where bucket increments when the observer notices a >30-min gap.
- **Symbol-level observation granularity.** Phase 2 observations stamp `file_path` only; `symbol_refs` is empty. `work_in_flight --match_mode=symbol` therefore doesn't surface observations — use `--match_mode=file`. Adding symbol granularity requires running SCIP queries over the changed file on each batch, which is a separate piece of work.

## Where things live

- `sovereign/crates/sovereign-work-atlas/src/observer.rs` — `AtlasObserver` (Phase 2 passive sensor).
- `sovereign/crates/sovereign-work-atlas/src/confidence.rs` — grade thresholds + `observation_grade()`.
- `sovereign/crates/sovereign-work-atlas/src/tools/broadcast.rs` — `ClaimBroadcaster` trait, `NullBroadcaster`, `DeferredBroadcaster` (Phase 2 indirection seam).
- `sovereign/crates/sovereign-mesh/src/work_atlas_broadcaster.rs` — `MeshBroadcaster` (Phase 2 real impl).
- `sovereign/crates/sovereign-mesh/src/daemon.rs::set_mesh_store` — Phase 2 hook so the daemon's `AppState.mesh_store` IS the work-atlas's store.
- `sovereign/crates/sovereign-cli/src/daemon_cmd.rs` — wire-up: observer registration, broadcaster swap-in, GC spawn.
- `sovereign/crates/sovereign-work-atlas/` — the crate.
- `sovereign/crates/sovereign-work-atlas/src/tools/resource_may_i.rs` — `resource_may_i` tool, `resource_verdict`, `DEFAULT_RESOURCE_TTL_SECS` (resource-commons convention, order seat-resource-commons).
- `sovereign/crates/sovereign-cli-llm/src/claim_cmd.rs` — `sovereign claim` dispatch incl. `may-i` / `take` (daemon-first; the in-process fallback lives here too).
- `commonwealth/crates/commonwealth-api/src/admission.rs` + `state.rs` + `routes_status.rs` — the per-peer request tally on `/status` (`inference.peer_requests`, order seat-resource-commons UC-R1).
- `commonwealth/crates/commonwealth-state/src/peer_preferences.rs` — `GOSSIP_EXCLUDED_APP_IDS` slice + paired test.
- `sovereign/crates/sovereign-mesh/src/gossip.rs::broadcast_now` — immediate fan-out helper.
- `sovereign/crates/sovereign-mesh/src/mcp_router.rs` — `X-Agent-Session` extraction.
- `sovereign/crates/sovereign-core/src/types.rs::ToolContext` — `agent_session_token` field.
- `sovereign/crates/sovereign-tools/src/code/blast_radius.rs` — `concurrent` field injection.
