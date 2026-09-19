# ring-doc — review findings

One row per finding: principle, path:line, fixed-in hash (or why not fixed).

## REVIEW-audit-rd-1 (2026-09-18, range d8cd7bb9f..18c7f405b)

| # | principle | path:line | fixed in | finding |
|---|---|---|---|---|
| 1 | 5 (gate) | sovereign/crates/sovereign-core/tests/main/f26_egress_census.rs:835 | e88a71212 | `routes_rail_live.rs` (6ac1fd39f) added an HTTP client construction site the F26 census did not know; TESTALL red. Registered Mesh, 1. |
| 2 | 1 | sovereign/crates/sovereign-api/src/routes_rail_live.rs:278 | e88a71212 | `offer_peer` gave up on a peer with no tracing event; the reason reached only the HTTP body. |
| 3 | 1 | sovereign/crates/sovereign-api/src/routes_rail_live.rs:308 | e88a71212 | `live_push`'s 413 and 422 refusals had no tracing event. |
| 4 | 1 | sovereign/crates/sovereign-api/src/routes_internal/ring_live.rs:53 | e88a71212 | the receiver's malformed-envelope refusal was silent while its two sibling refusals warn. |
| 5 | 8 | sovereign/crates/sovereign-grants/src/guest_grant.rs:105, sovereign/crates/sovereign-api/src/server.rs:271, sovereign/crates/sovereign-cli-shared/src/rail.rs:50 | not fixed | `/v1/rail/live` is spelled three times (grant scope, route, client const), extending the same triple spelling append/log already had before the queue. `sovereign-grants` does not depend on `sovereign-cli-shared`, so one const needs a home both layers can read — a design call, carried to NEEDS_HUMAN. Divergence is caught end-to-end by `ring_live_non_durable` (a guest grant drives the route). |
| 6 | 9 | sovereign/crates/sovereign-cli-llm/src/ring_cmd/dev.rs:148 | no change | `upstream` matches four string ops (smell: >3 arms). Kept: it is the one parse of a URL path segment into the route table, and a test asserts the table; an enum would move the same match into `FromStr`. |

Foreign reds seen by this audit (outside the campaign, not fixed here): see
the audit's NEEDS_HUMAN package — conformance tags stale since 30293904f,
`ingest_failure_modes::a_stopped_ingest_is_listed_but_not_usable`,
`every_journey_cites_a_doc_that_exists` (gitignored RING_APPLICATIONS.md
absent on the Halo), arch-gate (AGENTS.md +399 B, approach band +59; campaign
net 0), env-gate (`SOVEREIGN_SIDECAR_FEATURES` declared twice since e3474619c).


---

# ralph/REVIEW_FINDINGS.md — domains campaign

Each finding: principle · path:line · fixed-in. Recorded-not-changed entries
name the reason no code edit was made.

## REVIEW-audit-1 — rung 3, the instrument

Verified claims of the row:

- **Every axis carries a planted positive it catches and a planted negative it
  refuses (ARCH 5).** `python3 scripts/domains-census.py --self-test` exits 0,
  10 axes, 10/10 caught, 10/10 refused. Read each axis's `positive`/`negative`
  fixture: peer/shared-edges/word-owners/atom/crate-lines/misnamed/queue/
  congestion/liftable/plan all drive the axis's own `detect` over a temp dir,
  and each negative is a plausible near-miss (a `use`, a comment, a string, a
  lowercase local, an `impl`, a translated edge, an in-owner definition, a
  coverage-complete fixture, an all-own crate, a same-context commit, a
  kernel-tagged exempt crate, a valid tier window). Not tautological.
- **`scripts/daemon-route-census.py` names `sovereign/crates/sovereign-api/src`
  as a host, and a HOSTS entry whose directory is absent errors rather than
  returning zero** (dm3-route-census-hosts). Confirmed at
  `scripts/daemon-route-census.py:25` and `:100-103`.

Findings:

- **ARCH 8 (one decider, one name)** · `scripts/domains-census.py:330` ·
  `_is_member_edge` spelled the peer word as a literal `"Peer"` beside the
  module constant `_PEER_WORD`, so one word had two spellings. Fixed: use
  `_PEER_WORD`. Fixed in `2e5621ba6`.
- **ARCH 8 (one accessor per path)** · `scripts/domains-census.py:228` ·
  `peer_defs` read the repo registry via `registry()` while every sibling axis
  (`shared_edges`, `word_owners`, `atom_defs`, `misnamed`, `queue`, `liftable`,
  `predicate_problems`, `plan`) reads `registry_for(root)`; a fixture-planted
  `quality/DOMAINS.toml` would be ignored by this axis alone. Fixed: use
  `registry_for(root)`. Behaviour on the tree and on every existing fixture is
  unchanged (no peer fixture plants a registry). Fixed in `2e5621ba6`.

Recorded, not changed:

- **O3 "Seams" (no crate name, word or path as a constant)** ·
  `scripts/domains-census.py:171,174,325,490,702,703` · the script carries six
  axis-subject literals: `_PEER_WORD="Peer"`, `_COMMONWEALTH_PREFIX`, `_MEMBER_EDGE="MemberRecord"`,
  `_EXEMPT_CONTEXTS={"kernel","back-of-house"}`, `_ATOM_WORD="Atom"`,
  `_ATOM_EXCLUDED_ROOTS`. The row's claim ("the script carries no crate name,
  word or path as a constant") is false as literally written. Each literal is
  the axis's own subject or scope, is documented at its site, and is spelled in
  `domains-3-instrument`'s steps; the Seams' *purpose* — registry content
  (contexts, owned words, tags, dispositions, allow-list, homes) is DATA read
  from `quality/DOMAINS.toml` — holds. Moving them into the registry is a
  schema decision, not a behaviour-preserving audit fix, so it is recorded
  here rather than made.
- **ARCH 5 (a check with no failing input you can name)** ·
  `scripts/domains-census.py:2351` · `share_ok = (ctx in homes) or (reg_lines >= 0
  and t_lines[0] >= 0)`: the second disjunct is always true, so `plan` prints
  `share-monotone ok` for every cluster and the assert cannot fail. The site's
  own comment says it is "computed, not asserted"; the order asked for the
  assert and the move definition makes monotonicity structural.
- **ARCH 5 (a branch with no planted control)** ·
  `scripts/domains-census.py:1549` `_run_lift` · the run tier of `liftable` is
  exercised by no control (the axis's controls use `run_lifts=False`), and no
  bar sets `timeout_s`, so the branch is dark today.

## Pre-existing red gates — CLEARED by the supervisor resolution

`TESTALL` and `PREPUSH` were red on this tree for reasons that predate rung 3
and were already public on `origin/main`; none was touched by
`origin/main..HEAD`. The resolution session cleared them, none by weakening a
campaign bar:

- **`arch-gate`** — the baselines were stale against `origin/main` (`AGENTS.md`
  47,980 → 48,379 bytes; approach band 200,868 → 200,927 lines). Re-pinned at
  `origin/main` (this branch changes no `.rs` and no `AGENTS.md` byte), ledgered
  in `sovereign/SYSTEM_OVERVIEW.md` §10.1w.
- **`env-gate`** — a duplicate `SOVEREIGN_SIDECAR_FEATURES` row, a merge of
  `69ab4a68f`'s `cli-binaries` copy with `739735496`'s `dev-gates` one. The
  stale `cli-binaries` copy removed; `docs/ENV_FLAGS.md` regenerated.
- **`sovereign-lint-scoped`** — `scripts/sovereign-lint.sh` hardcoded
  `corpus-engine/treesitter`, a hard cargo error once `sovereign-desktop` dropped
  its corpus-engine dependency at svt-6. It now uses `resolve_features` from
  `scripts/lib/cargo-scope.sh`, the shared decider (ARCH 8).
- **`TESTALL`** — two stale/flaky tests against their deciders:
  `ingest_failure_modes::a_stopped_ingest_is_listed_but_not_usable` cleared only
  `indexes_built` and left the three sub-index flags true, an impossible state
  `index::readiness::indexes_searchable` correctly calls searchable; it now
  clears all four, matching `reset_for_resume`. `commonwealth-media`'s
  `a_renewed_claim_outlives_its_original_ttl` raced a 40 ms TTL against a
  `thread::sleep` floor; the margins widened, the assertion unchanged.
  `quality/conformance/sovereign-core.toml` line pins refreshed to match
  `quote_verification.rs`.

## REVIEW-audit-2 — wave 0

Range audited: `git log 0d7261da2..HEAD` (the previous audit's hash), the
wave-0 units plus the census-constants/checks rows. Checks: TESTALL exit=0
(13312 pass, 0 fail), PREPUSH exit=0.

Findings, fixed:

- **ARCH 8 (one decider, one name)** · `sovereign-core/src/runtime.rs:674` ·
  wave 0 introduced `PrincipalScope` and made a wired resolver's `None`
  refuse, but left the resolver→scope mapping inline in
  `Runtime::principal_scope`. One decision, two homes once a test needs it.
  Fixed: the mapping is one named function,
  `PrincipalScope::from_resolver` (`context.rs:55`), beside the type it
  constructs; `principal_scope` delegates. Fixed in `7f1f74501`.
- **ARCH 5 (a branch with no planted control)** ·
  `sovereign-core/tests/main/core_tests.rs:575` · the mapping's three arms
  were exercised by nothing: the refusal test constructs
  `PrincipalScope::Unresolved` directly and never enters `from_resolver`.
  Fixed: `principal_scope_from_resolver_has_three_distinct_arms` drives all
  three with an input each would fail on if the arms collapsed. Fixed in
  `7f1f74501`.
- **ARCH 3 (write for the next reader)** · four doc claims wave 0's
  behaviour change falsified, fixed in the same pass:
  `sovereign-contracts/src/traits.rs:96` (`PrincipalResolver` said a `None`
  "means no tenancy … no corpus is hidden"; it now refuses),
  `sovereign-server/src/tenant.rs:97` (unprefixed id "no scoping" → the turn
  refuses), `sovereign-core/src/runtime.rs:341` (`corpus_principal` field) and
  `:442` (`RuntimeParts` "does NOT yet fix" bullet now records the daemon
  resolves `sensitive_corpora`), `sovereign-runtime-recipe/src/lib.rs:392`
  (host-override example omitted the daemon). Fixed in `7f1f74501`/`e2e51f64c`.
- **ARCH 3/4 (a comment naming a path that no longer owns the fact)** ·
  three non-code references still named the old shim path
  `sovereign_api::openai_types` after dm-wire-openai-types moved the wire
  vocabulary to oicp-types: `oicp-types/src/completion.rs:391`,
  `sovereign-inference/src/embedded/grammar.rs:43`,
  `quality/DOMAINS.toml:695`. Repointed to the canonical home. Fixed in
  `56b578f1a`.
- **Gate: `arch-gate` approach band GREW 200927 → 200932 (+5, a hard gate)** ·
  the wave-0 code growth (`runtime.rs` +13 net, `runtime/turn.rs` -8) plus the
  audit's own doc additions. Fixed behaviour-preservingly — the same facts in
  the original line count — not with `--update-baseline` (PROMPT §7). Band
  back to 205 files / 200927 lines. Fixed in `e2e51f64c`.

Recorded, not changed:

- **`size-gate` (advisory)**: 37 keys grew, e.g. `sovereign-mesh::tests`
  +1700. This is the campaign's own growth and the gate is `warn_gate` by
  design (AGENTS.md: promote after a week with no false positive). Not
  re-pinned — that would absorb the growth the gate exists to surface. It does
  not block PREPUSH (which exits 0).
- **`concept-gate` could-not-judge (exit 3, declared)**: a verdict, not a
  failure; the pre-push runner counts it as attention, not blocking.

Verified claims of the wave-0 rows (spot-checked against the tree, ARCH 4):
the two MOVE rows keep the old path compiling via the shim
(`sovereign-api/src/lib.rs:35,38`) and name the moved files' real imports
(`oicp_types::openai_types`, `crate::requirements`, `crate::completion`);
`dm-serving-dead-types`/`dm-serving-meshplan-cascade` deleted only types whose
every hit was an in-crate definition, re-export or test (CALLERS proof in each
commit body); `REVIEW-build-knowledge-assignment` moved the planner to
`sovereign-grants` and dropped `corpus-engine` from `sovereign-serving`, with
the registry note and SYSTEM_OVERVIEW updated in the same commit.

## REVIEW-audit-3 — the pod moves

Range audited: `git log 7f1f74501..HEAD` (the previous audit's hash), the
seven pod units. Checks: TESTALL exit=0 (13312 pass, 0 fail); PREPUSH exit=1,
one blocking gate (arch-gate) — §6, see `ralph/NEEDS_HUMAN.md`.

Findings, fixed:

- **ARCH 8 (one decider, one name)** · `sovereign-pods/src/worker_daemon.rs:89`
  · `EchoRunner` spelled the `SystemTime -> secs` conversion inline while the
  other three pod modules already call `sovereign_time::unix_now_u64`
  (`worker_http.rs:278`, `worker_subprocess_runner.rs:663`,
  `worker_pod.rs:417`). Fixed: the shared decider. Fixed in `a4b5e7756`.
- **ARCH 3 (the doc lands with the code)** · `quality/DOMAINS.toml:886,905` ·
  the clock fix removed three lines (291 -> 288) and moved
  `InferenceProxyConfig` to `worker_daemon.rs:156`, staling the `[[module]]`
  row and the `worker_inference_proxy` note's cite. Fixed: both re-keyed in
  the same pass; the note's other cites verified against the tree
  (`worker_http.rs:214,817`, `worker_inference_proxy.rs:67`,
  `sovereign-cli-daemon/src/daemon_cmd/worker.rs:82,120`). Fixed in
  `5db144fec`.
- **Shims whose importers are all repointed (the row's VERB)** · the six pod
  shims were deleted and `sovereign-cli-llm` / `sovereign-cli-daemon`
  repointed at `sovereign_pods::`; `worker_pod`'s shim stays because
  `pinned_pod_snapshot.rs:47`, `pinned_transport.rs:44` and
  `pinned_worker_source.rs:56` still resolve `crate::worker_pod` through it.
  Verified: `git grep 'sovereign_mesh::worker_(controller|daemon|http|inference_proxy|subprocess_runner)|sovereign_mesh::multi_pod_coordinator' -- '*.rs'` is empty. Fixed in
  `38b0d37fa`.

Recorded, not changed:

- **Frozen cluster-graph cites name the old mesh paths** ·
  `quality/DOMAINS.toml:5230-5231,5459-5460` · four `cites` in the sovereign-mesh
  cluster graph (measured 2026-09-14, banner at `:4509`) point at
  `sovereign-mesh/src/worker_daemon.rs:158` and `.../worker_http.rs:214`. They
  are the 2026-09-14 measurement's own coordinates, not live pointers; the
  banner at `:3818` says module rows were re-tagged after the measurement. A
  re-key would falsify the measurement, so it is recorded here (ARCH 3/4).
- **`size-gate` (advisory)** · 37 keys grew (`sovereign-cli-daemon::tests`
  +220, `sovereign-cli-llm::tests` +426, `commonwealth-media` +429, …). This is
  the campaign's own growth and the gate is `warn_gate` by design (AGENTS.md).
  Not re-pinned; it does not block PREPUSH.

Resolved by the supervisor resolution (attempt 1):

- **`arch-gate` — two NEW oversized files at moved paths, re-keyed.** The pod
  moves `git mv`'d `worker_http.rs` (2046) and `worker_subprocess_runner.rs`
  (1271) from `sovereign-mesh/src/` to `sovereign-pods/src/`; `oversized.txt`
  is keyed by path, so the two frozen rows stopped matching and the move read
  as new debt at unchanged counts. Fixed: both rows re-keyed to
  `sovereign-pods/src/` with their counts unchanged — §10.1d "path re-key, no
  debt", the `atoms.rs` precedent (`9722bf821`), not `--update-baseline`.
  Ledgered `sovereign/SYSTEM_OVERVIEW.md` §10.1x. The MOVE recipe
  (`ralph/PROMPT.md` §3a) gained step 6, which carries a moved file's baseline
  row in the same commit, so wave 1's remaining moves (`peer_inference.rs`
  5,399 and the rest) do not re-open it. `arch-gate` exit=0.

## REVIEW-audit-5 — the scheduler cycle

Range audited: `git log f067f7571..HEAD` (the previous audit's hash), the
scheduler mint and its six rows, the pod-to-`sovereign-scheduler` move and
the two splits. Checks: TESTALL exit=0 (13312 pass, 0 fail); PREPUSH exit=1,
**one blocking lane — `boundary-gate` — which is the `serving` package's
declared-red**, approved 2026-09-15 (`ada68c8cd`) and pasted verbatim by
`dm-serving-package-rows` (`c219d4a62`). Its five violations are unchanged and
this audit adds none: `sovereign-serving → commonwealth-{core,state}`, the
`lib.rs:7` embed inside a COMMENT, and the two host `[[exception]]` rows that
are stale-by-construction until the host half lands. O9's own gate line is
`./scripts/pre-push.sh  # everything else green`; every other lane passed
(`size-gate` is the only other and it is an advisory `warn_gate`). Recorded,
not "fixed": the red is the approved state, not a defect this audit may clear
without drawing the boundary in the wrong place (K4).

Findings, fixed (all in `adc29b1ba`):

- **ARCH 3 (the doc lands with the code)** · `quality/DOMAINS.toml` · the nine
  `[[module]]` rows for the moved modules still keyed `sovereign-mesh/src/`,
  and the four new files (scheduler `lib.rs`, host `lib.rs`/`recorder.rs`/
  `slot_select.rs`) had no row. Re-keyed and added; line counts re-measured
  (`decision_log` 1527→1254 after the sink split, `oicp_select` 946→664 after
  the slot-pick split, `decision_trace` 634→640; `scheduler_core`'s note
  updated for the `pub` boundary items).
- **ARCH 3 (the doc lands with the code)** · `sovereign/SYSTEM_OVERVIEW.md` ·
  the §8 module tables, the "Understand OICP routing" row and the mesh-sim
  scoreboard line still named `sovereign-mesh/…`; re-keyed to
  `sovereign-scheduler/`.
- **ARCH 3/4 (a path that no longer owns the fact)** ·
  `quality/conformance/sovereign-scheduler.toml` (NEW, generated) ·
  `conformance_tags_are_fresh` was red until the yield_backoff FE-106 claim
  followed the file out of `sovereign-mesh.toml`; regenerated.
- **ARCH 3/4 (path-keyed registries)** · `quality/conformance-specs.toml:435-437,
  741-742` · FE-106 (`yield_backoff` :79/:195) and IN-2 (`slot_aliases`
  :88/:126) still keyed the mesh paths, and FE-106's `landed` named the
  `sovereign-mesh::` binary; re-keyed to `sovereign-scheduler`. The canon note
  `.canon/sources/notes/invariant/c719398a.md` names this file as a
  non-baseline path key that must be re-keyed on a move.
- **ARCH 3/4 (path-keyed registries)** · `quality/sabotage/fe-dst.toml:257`,
  `quality/sabotage/fe-dst-mesh.toml:252` · the `fe-15-a` mutant's `target`
  still named the mesh `yield_backoff.rs`; repointed, or the mutant can never
  be planted (the `all.toml` precedent from `928ec78a4`).
- **ARCH 3/4 (a doc naming a path that no longer owns the fact)** ·
  `docs/CMNWLTH_DESIGN.md:114`, `docs/LAZY_INFERENCE_ON_THE_RAIL.md:39`,
  `sovereign/SYSTEM_OVERVIEW.md:7695`, `sovereign/docs/specs/VERIFIER_V0.md:414`,
  `research/smb-onprem-adoption/MULTI_TENANT_SMB_ADOPTION.md:193`, and the
  `sovereign-contracts/src/types/next_edit_journal.rs:26` doc comment.
  Repointed to the canonical home (the audit-2 precedent: a shim path in a doc
  is still a stale pointer once the owner moves).

Recorded, not changed:

- **Frozen measurement coordinates** · `quality/DOMAINS.toml`'s `[[noun]]`
  `file` fields, `[[collision]]` `definitions`, and the `[[cluster]]` graph
  `cite`/`file` rows still name the mesh paths; so do
  `sovereign/docs/specs/SCHEDULER_QUALITY.md:825,964` (dated 2026-07-26
  measurement blocks). They are the measurement's own coordinates at
  measurement time, not live pointers; a re-key would falsify the record
  (ARCH 3/4; the audit-3 precedent for the cluster graph).
- **The shims stay** · the audit's VERB ("delete any shim whose importers are
  all repointed") does not fire: every `sovereign_mesh::{scheduler_core,
  oicp_select, decision_log, decision_replay, decision_trace, predicted_time,
  yield_backoff, tier, slot_aliases}` shim still has live importers — the
  mesh host modules (`peer_inference`, `inference_adapter`, `oicp_synthesis`,
  `throughput_tracking`, `mesh_sim`) resolve them through `crate::<m>`, and
  the mesh integration tests through `sovereign_mesh::<m>`. They are deleted
  when the host half moves (`REVIEW-mint-serving-host`).
- **`yield_backoff`'s monotonic clock read** (`yield_backoff.rs:81,85,118`) ·
  recorded as a residual by `REVIEW-build-sched-move` (SB "Corrected
  2026-09-14" bullet 1); the rewrite to take `now` is owed, not done here.
- **`size-gate` (advisory)** · 41 keys grew, the campaign's own growth; the
  new crates read as "new and unbaselined". `warn_gate` by design, does not
  block PREPUSH. Not re-pinned.

## REVIEW-audit-6 — the knot's ports

Range audited: `git log e21ac9c88..HEAD` (the previous audit's hash), the
seven rows that landed the knot's ports — venue, guest-port, worker-port,
emitter-port, manifest-port, local-inference, serving-principal. Checks:
TESTALL exit=0 (13327 pass, 0 fail); PREPUSH exit=1, **one blocking lane —
`boundary-gate` — the `serving` package's declared-red**, five violations
unchanged from audit-5 (`sovereign-serving → commonwealth-{core,state}`, the
`lib.rs:7` embed, and the two host `[[exception]]` rows that are
stale-by-construction until the host half lands). `arch-gate` was red on two
files and is green after the splits below; `size-gate` is advisory;
`concept-gate` is could-not-judge on a stale graph, as audit-5 recorded.

The row's two claims, verified:

- **Every port carries a planted positive and a planted negative (ARCH 5).**
  `VenueSource` — `OnePeer` (a candidate routes) vs `NoPeers` (empty falls
  local), `peer_inference.rs` tests; `VenueHost` — `LedgerHost` (a gossiped
  peer emits) vs `NoPeers` (absence, never a defaulting emitter) and a pinned
  venue (`peer_inference.rs:4252`); `GuestLenderSource` — `a_live_grant_…`
  vs `a_node_with_no_link_…` and `an_unusable_link_is_not_an_absent_one`
  (`sovereign-serving-host/src/guest_lender.rs:128`); `WorkerState` —
  `a_registered_pod_resolves_its_state` vs `an_unregistered_pod_is_absent`
  (`worker_state.rs:40`); `LedgerEmitter` — `a_peer_outcome_with_tokens_mints_one_fact`
  vs three negatives (`ledger.rs:66`); `SlotManifest` —
  `an_annotated_file_resolves_capabilities_and_size` vs
  `an_unannotated_file_is_absent` (`slot_select.rs:365`); `Principal` — the
  five distinct keys vs `only_a_member_has_a_peer_key`
  (`sovereign-contracts/src/principal.rs:152`); `LocalInferenceService` —
  `StubFim` vs `NoFim` (`sovereign-api/src/routes_completions.rs:532`).
- **The three `[[exception]]`-shaped risks resolve with no third exception
  (K4).** `worker_pod` — the pod wire protocol moved to the shared leaf
  `sovereign-contracts::worker_pod` (`worker_state.rs:57`); `models_manifest`
  — the `SlotManifest` port replaces the six `DEFAULT_MANIFEST` reaches;
  `commonwealth_state` — the `LedgerEmitter` port, the daemon implementing
  it. `quality/ARCH_LAYERS.toml:1240,1247` still carries exactly the two
  grandfathered serving rows; none added.

Findings, fixed:

- **ARCH §3.1 (trim or split)** · `peer_inference.rs`, `traits.rs` · the
  ports added since audit-5 pushed both past arch-gate's 50-line slack (5609
  vs 5449; 2155 vs 2086). Split out `venue_host.rs` (the Fabric-side ports)
  and `local_inflight.rs` (the RAII guards) from `peer_inference`, and
  `local_inference.rs` from `traits.rs`, every historical path re-exported.
  Fixed in `f1f50aab6`.
- **ARCH §3.1 (trim or split)** · `sovereign-api/src/routes_inference.rs`,
  `sovereign-mesh/src/inference_adapter.rs` · the `InferenceProvider`
  collapse grew both past slack (2408 vs 2392; 2155 vs 2136). The route
  file's two test modules and the adapter's two move to sibling files via
  `#[path]`, so every module path and `use super::*` is unchanged. Fixed in
  `0f55c49c4`.
- **ARCH 3 (the doc lands with the code)** · `quality/DOMAINS.toml` · the new
  `principal.rs` had no `[[module]]` row, so `crate-lines --crate
  sovereign-mesh` exited 4 on a coverage hole (ARCH 6 — the count would have
  silently understated); the split files needed rows; and 19 line counts the
  audited commits left stale were re-measured. Fixed in `f1f50aab6`
  (splits) and `0f55c49c4` (the four test files).
- **ARCH 3/4 (path-keyed registry)** · `quality/conformance/sovereign-mesh.toml`
  · the UI-22 line followed `daemon.rs`'s test when `PeerInferenceEndpoint`
  left the file. Fixed in `f1f50aab6`.

Recorded, not changed:

- **`boundary-gate` (blocking, approved)** · the `serving` package's five
  declared-red violations are unchanged from audit-5 and are the state
  `HUMAN-design-review` approved; this audit adds none (K4 — clearing them
  here would draw the boundary in the wrong place).
- **`concept-gate` could-not-judge** · the SCIP graph is stale (indexed
  `f1f50aab`, HEAD `0f55c49c`); re-index is `svrn project refresh`, not this
  unit's work. Audit-5 recorded the same.
- **`size-gate` (advisory)** · 41 keys grew, the campaign's own growth; the
  new crates read "new and unbaselined". `warn_gate` by design.

## REVIEW-audit-7 — the knot's moves

Range audited: `git log be0cfbd9a..HEAD` (the previous audit's hash) — the
serving-host moves `dm-serving-move-leaves`, `REVIEW-build-serving-move-synthesis`,
`-move-throughput-guest`, `-move-adapter`, `-move-peer`, `-move-admission`.
Check: TESTALL exit=0 (13343 pass, 0 fail). PREPUSH rides the wave-close
`REVIEW-audit-8` per the row. All in `d43188ff9`.

The row's two claims, verified:

- **No module the package holds names `sovereign-mesh` (rule 5), `sovereign-api`
  (rule 4) or `sovereign-core`.** `cargo xtask boundary-gate` reports exactly
  three violations, all the peg `sovereign-serving`'s (`commonwealth-core`,
  `commonwealth-state`, the `lib.rs:7` `include_str`) — none the host's. Rule 4
  and rule 5 edges are zero.
- **The two grandfathered `[[exception]]` rows now match real edges.**
  `sovereign-serving-host → sovereign-inference` and
  `sovereign-serving-host → commonwealth-core` are live `[dependencies]`
  (`sovereign-serving-host/Cargo.toml:36,:35`); boundary-gate reports neither as
  stale, and no third serving `[[exception]]` was added (K4).

Findings, fixed:

- **ARCH 3/4 (a pointer to a path that no longer owns the fact)** · the moves
  left 18 `.rs` comment lines and four docs naming the moved modules under
  `sovereign_mesh::` / `sovereign-mesh/src/` (inference_adapter, peer_inference,
  model_fetch, guest_lender, oicp_synthesis, tool_profile,
  source_content_validator, worker_eligibility, fim_adapter). Repointed to
  `sovereign_serving_host::` / `sovereign-serving-host/src/` across cli-dev,
  cli-daemon, cli-llm, contracts, core, enrichment-build, inference, tools,
  commonwealth-core, corpus-engine, oicp-types; and
  `docs/RPC_DISTRIBUTED_INFERENCE.md:226`,
  `docs/LAZY_INFERENCE_ON_THE_RAIL.md:45`, `quality/NOUN_CONVERGENCE.md:1111`,
  `quality/env-flags.toml:570` (+ regenerated `docs/ENV_FLAGS.md`). Fixed in
  `d43188ff9`.
- **ARCH 3/4 (path-keyed registries)** · `quality/conformance-specs.toml`
  FE-105/FE-133/FE-135/IN-5 and `quality/sabotage/all.toml` `ci-43` still keyed
  the mesh paths; re-keyed to `sovereign-serving-host`, line numbers
  re-measured. Fixed in `d43188ff9`.
- **ARCH 3 (a comment citing a unit that did not do this)** ·
  `sovereign-mesh/src/lib.rs` · the `throughput_tracking` shim carried a second
  attribution `// shim: moved by domains dm-sched-move-tier`, but that unit
  moved `tier`; removed. Fixed in `d43188ff9`.
- **Shims whose importers are all repointed (the row's VERB)** · five deleted:
  `local_inflight`, `oicp_synthesis`, `prompt_compactor`, `pinned_transport`,
  `yield_backoff`. A word-boundary count of `sovereign_mesh::<m>` over the
  workspace and `crate::<m>` over `sovereign-mesh` gave 0/0 for each. The
  remaining shims keep live importers (the daemon's `sovereign_mesh::peer_inference`
  sites wait on `REVIEW-build-serving-repoint-daemon`). Fixed in `d43188ff9`.
- **TESTALL red — `conformance_tags_are_fresh`** · `quality/conformance/
  sovereign-api.toml` line 675 → 676 (the admission move shifted a
  `routes_status.rs` test). Regenerated: FE-133/FE-135 followed
  `worker_eligibility` into the new `quality/conformance/sovereign-serving-host.toml`,
  and the UI-22 (`daemon.rs`) pin moved 5283 → 5285. Fixed in `d43188ff9`.

Recorded, not changed:

- **Frozen measurement coordinates** · `quality/DOMAINS.toml`'s `[[noun]]`
  `file` fields, `[[collision]]` `definitions`, and the `[[cluster]]` graph
  `cite`/`file` rows still name the mesh paths; they are the 2026-09-14
  measurement's own coordinates, not live pointers (the audit-3/5 precedent). A
  re-key would falsify the record.
- **FE-105's `landed` test name resolves nowhere** ·
  `quality/conformance-specs.toml` · `our_own_shed_refusals_never_quarantine_the_peer_that_shed`
  is absent from the tree, and was already absent at `be0cfbd9a` and in the
  pre-move file (`10f757158^`): a pre-existing over-claim (`status = "landed"`
  with no tagged test), not this range's doing. The path fields were re-keyed;
  the name is left for the operator, since no test covers `book_peer_failure`'s
  shed exemption anywhere (ARCH 5).
- **`SERVING_BOUNDARY.md` (e)'s signature sketch predates the ports** · the
  `with_peer_source{,_and_publisher}(raw, src[, publisher])` line is now
  `with_peer_source(raw, source, host, manifest)` /
  `with_peer_source_and_publisher(..., publisher)`
  (`sovereign-cli-daemon/src/daemon_cmd/provider.rs:119,132,142`). The one stale
  type name (`PeerEndpointSource` → `VenueSource`) is fixed in `d43188ff9`; the
  signature is guidance for the still-pending `REVIEW-build-serving-repoint-daemon`,
  which re-measures its sites at premise-check time.
- **`boundary-gate` (blocking, approved)** · the `serving` package's three
  remaining violations are the peg `sovereign-serving`'s, the state
  `HUMAN-design-review` approved; `REVIEW-build-serving-empty-peg` empties them.
  This audit adds none (K4).
- **`size-gate` (advisory)** · the campaign's own growth; `warn_gate` by design,
  does not block PREPUSH. Not re-pinned.

## Resolution 2026-09-16 — the lift's RUN half lands, and the closure defect it exposed

The supervisor halted `domains` at "3 iterations without a commit" on
`REVIEW-build-serving-lift-harness`. The work was coherent and passing; what
blocked a commit was that `scripts/serving-lift.sh --sandbox` could not reach the
harness it had just been given. Committed as `f1f4b7b7f`.

**ARCH 6 (never silently substitute) — steps 5-8 were made real, and the lift
cannot reach them.** Verified by `cargo tree` on 2026-09-15:

    cargo tree -p sovereign-serving-host -i corpus-engine
      corpus-engine <- sovereign-core <- sovereign-inference <- sovereign-serving-host
    cargo tree -p sovereign-serving-host -i llama-cpp-4
      llama-cpp-4 <- sovereign-inference <- sovereign-serving-host

The grandfathered `sovereign-serving-host -> sovereign-inference`
`[[exception]]` edge puts the whole inference stack in the package's closure, so
`scripts/serving-lift.sh --sandbox` writes verdict 0 at step 2 —
`corpus-engine/build.rs:59` requires the sibling `sovereign-recipes/` tree — and
would write 0 at step 4 (`llama-cpp-4` present). That contradicts SB "The two
tiers" and the lift's own step 4 ("the inference stack are deliberately outside
this closure"). The exception's tracking says it clears when "the remote provider
is reached through `oicp-client`", which is already true for
`sovereign_inference::remote` (`pub use oicp_client::*`); the remaining reaches
(`embedded/grammar.rs`'s tool-call parsing, `fim.rs`) were not counted when the
exception was written.

The plan's premise — that the lift reaches verdict 1 after the peg empties — was
false. Row and source order corrected together:
`REVIEW-build-serving-drop-inference` is minted (the burn-down, with the
`f573999ee` / order ei-5a-build-cut precedent named in the row) and
`DEMO-d1-serving-lift` now depends on it. `quality/DOMAINS.toml`'s `serving.lift`
comment named the old cause and now names this one.

**Second, independent blocker, folded into the same row: the lift's toolchain.**
The sandbox runs the host's default rustup toolchain (1.94.1) while the copied
`[workspace.package] rust-version = "1.95"` is inherited by every package crate,
so `cargo build` fails on the MSRV check before any closure is judged. The lift
must run the sandbox on a toolchain satisfying the declared MSRV or ABSTAIN
(exit 3) naming the toolchain — a measured `{"value": 0}` from an inadequate
toolchain is the substitution ARCH §18.2/§18.3 forbids.

**Honest status of the harness row.** Its verb (write the harness, flip steps
5-8) is done and its `LINT` and `TEST(sovereign-serving-host)` checks pass; its
lift check yields exit=0 with the lift's own verdict 0, the same shape the
`dm-serving-lift-skeleton` row was marked on. What is NOT proven is that the lift
ever RUNS steps 5-8 — that is now `DEMO-d1-serving-lift`'s job, and it cannot
happen until the burn-down row lands. The row is marked done on its verb; the
lift's verdict-1 requirement was not weakened and no `[x]` was written on a check
that fails.

## REVIEW-build-serving-drop-inference — the inference edge leaves, and the embed it exposed

VERB: drop `sovereign-serving-host`'s `sovereign-inference` edge, remove the
grandfathered `[[exception]]` row, and make the lift measure on a toolchain that
satisfies the declared MSRV.

Landed:

- **The host's four non-inference reaches, each repointed or moved by what it is
  (ARCH 8 — a move, never a copy, re-exported at the old path so no caller changed).**
  `sovereign_inference::remote::{RemoteApiProvider, EndpointResolver}` →
  `oicp_client::` (the path swap the exception's `tracking` named);
  `sovereign_inference::embedded::{ParsedToolCall,
  escape_unescaped_control_chars_in_string_values, parse_tool_calls_with_errors}`
  → `oicp-types/src/tool_calls.rs` (pure `serde_json` + `std`, the OpenAI wire
  leaf); `sovereign_inference::fim::{decide_mode, Feed, FimMode, FimStopTracker,
  StopOutcome, markers_for, build_fim_prompt}` → `sovereign-contracts/src/fim.rs`
  (arithmetic over `FimStyle`, already there). The llama halves — grammar.rs's
  GBNF and `detect_fim_style` — stay in `sovereign-inference`, re-exporting the
  moved items at `sovereign_inference::{embedded,fim}::*`.
- `sovereign-serving-host/Cargo.toml`: `sovereign-inference` out, `oicp-client` in.
- `quality/ARCH_LAYERS.toml`: the `sovereign-serving-host → sovereign-inference`
  `[[exception]]` row deleted (the edge is gone; a stale row fails the gate). One
  serving exception left.
- `scripts/serving-lift.sh`: an MSRV precondition before the build. The sandbox
  inherits `rust-version = "1.95"` but not `rust-toolchain.toml`, so it built on
  the host default 1.94.1 and step 2 wrote a MEASURED 0 from a toolchain the
  instrument cannot measure. It now selects an installed toolchain satisfying the
  MSRV (`1.95.0` here) or ABSTAINS (exit 3) naming both. Verified: the sandbox ran
  on `1.95.0` and step 2 compiled.

NEW FINDING, recorded not fixed — the lift still writes 0, for a cause the row did
not name. With the inference edge gone, `cargo tree -p sovereign-serving-host -i
llama-cpp-4|corpus-engine` is empty (the row's check) and the closure builds until
`sovereign-contracts` — a SHARED LEAF, not a package member — fails on two
`include_str!`s that ESCAPE its crate root:

    sovereign/crates/sovereign-contracts/src/recipe/registry.rs:31
      include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../../sovereign-recipes/registry.toml"))
    sovereign/crates/sovereign-contracts/src/recipe/schema.rs:25
      include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../../sovereign-recipes/schema/recipe_schema_descriptor.json"))

The flat-copy sandbox (the lift's design; a layout-preserving one is the smell
`studio/BOUNDARY.md` records) cannot resolve them, so step 2 fails with
`couldn't read …/sovereign-recipes/registry.toml`. This is the embed
`SERVING_BOUNDARY.md` "What a green gate does not prove" warned the lift would
find; the embedder is the shared leaf, so fixing it is a design decision (where
the two recipe artifacts live so a liftable leaf can embed them, without a second
copy — ARCH 8), not a behaviour-preserving move. `DEMO-d1-serving-lift` cannot
reach verdict 1 until it is decided. `quality/DOMAINS.toml`'s `serving.lift`
comment and `SERVING_BOUNDARY.md` now name this cause.

Checks: LINT exit=0 (workspace, `--all-targets`, 0 errors); LAYER exit=0; BOUNDARY
exit=0 (serving green, one exception); `cargo tree -p sovereign-serving-host -i
llama-cpp-4` and `-i corpus-engine` both print no reverse tree (the package is
absent from the closure); TEST(sovereign-serving-host) exit=0 (226 pass);
TEST(sovereign-mesh) exit=0 (831 pass); TEST(oicp-types) 189, TEST(sovereign-contracts)
423, TEST(sovereign-inference) 439 — all 0 fail. `scripts/serving-lift.sh --sandbox`
exit=0, VERDICT 0 (the embed above), on toolchain 1.95.0.

## REVIEW-audit-8 — the serving knot, end to end

Range audited: `git log 731f78548..HEAD` (the previous audit's hash) — the peg
empty, the daemon repoint, the lift harness, the inference-edge drop, the
leaf-embed injection seam, and the D1 demo. Checks: TESTALL exit=0 (13345 pass,
0 fail); PREPUSH exit=0 (arch-gate, clock-gate, rustfmt cleared below; size-gate
is the advisory `warn_gate`).

The row's claims, verified:

- **`boundary-gate` GREEN for `serving` with at most the two grandfathered
  exceptions.** `cargo xtask boundary-gate`: "serving 3/3 crates present … ✓
  every declared package reaches only itself + the shared leaves", exit 0. The
  `sovereign-serving-host → sovereign-inference` exception was deleted by
  `REVIEW-build-serving-drop-inference`; `quality/ARCH_LAYERS.toml:1238-1250`
  carries the one remaining grandfathered row, and the gate reports no stale one.
- **`crate-lines --crate sovereign-mesh` drops by the serving total.**
  `[[module]]` rows: 74,504 lines at `be0cfbd9a` → 59,692 at HEAD (−14,812); the
  serving-tagged total gone is 13,696. The extra −1,116 is the two other files
  the same moves took — `fim_adapter.rs` (745, tagged `workbench`) and
  `tool_profile.rs` (585, tagged `host`) — less `guest_source.rs` (184) and
  `slot_manifest.rs` (35) added. `crate-lines --crate sovereign-mesh` now
  reports **zero** serving-tagged rows (the O10 "no serving module remains").
- **`misnamed` shows sovereign-mesh's Fabric share rising.** 13,319/74,504 =
  17.9% at `be0cfbd9a` → 13,499/59,692 = 22.6% at HEAD.

Findings, fixed:

- **ARCH 3 (the doc lands with the code)** · `quality/DOMAINS.toml` · the audited
  commits left 39 `[[module]]` line counts stale against the tree (`daemon.rs`
  5611→5613, `peer_inference.rs` 5408→5493, `time.rs` 75→17 after the wave-0
  re-export, the peg's `inference_plan.rs` 135→90, …). Re-measured with `wc -l`;
  `crate-lines`/`misnamed` read these counts, so the instrument was reporting a
  stale snapshot. Fixed in `a7dc45146`.
- **ARCH 3/4 (a note citing a path that no longer owns the fact)** ·
  `quality/DOMAINS.toml` · three live `[[module]]` notes named deleted or moved
  homes: `daemon.rs`'s "holds PeerInferenceEndpoint (:4783)" (the type is now
  `sovereign_scheduler::venue::InferenceVenue`, re-exported at `:25`; the
  translation is `:2280-2345`), `routes_internal/gossip.rs`'s
  `sovereign_serving::InferencePlan` (now
  `commonwealth_state::inference_plan::InferencePlan`), and `state.rs`'s
  `sovereign_serving (:21-23)` (now `sovereign_serving_host::admission`).
  Repointed. Fixed in `a7dc45146`.
- **ARCH 5 (a check whose bar is the machine, not the subject)** ·
  `sovereign-compute/src/supervisor.rs:1327` ·
  `brief_healthy_stretches_do_not_reset_the_breaker` drained on a 1500 ms
  per-event deadline; under a full-workspace run's spawn/reap load the two crash
  cycles took longer, the drain returned `[Starting, Healthy]`, and TESTALL went
  red for a supervisor that was working correctly. It now uses the existing one
  decider `drain_states_until`, stopping when the breaker trips (30 s is a
  hang-detector), the shape its sibling took on 2026-08-14. Assertions unchanged.
  Fixed in `e4b7a429b`.
- **ARCH 3.1 (trim or split)** · `sovereign-serving-host/src/peer_inference.rs` ·
  the builder `REVIEW-build-serving-repoint-daemon` added pushed the file past
  arch-gate's 50-line slack (5399 → 5493). Split two ways: the `InferenceProvider
  for InferenceRouter` impl (739 lines) to `peer_inference/provider_impl.rs`
  behind `#[path]`, and the `InferenceRouterBuilder` to `router_builder.rs`
  (re-exported at the old path). Parent now 4692. Fixed in `6e7ebd3ed` (provider
  impl) and this commit (the builder + the `#[path]` wiring `6e7ebd3ed` omitted —
  see the recorded-not-changed note).
- **clock-gate** · `sovereign-serving-host/tests/main/serving_lift_harness.rs` ·
  the harness hand-read `SystemTime::now()`; it now asks the decider,
  `sovereign_time::unix_now_u64()`. Fixed in this commit.
- **The row's VERB — delete any shim whose importers are all repointed.** Four
  mesh shims deleted (`entry_endpoint`, `fim_adapter`, `source_content_validator`,
  `tool_profile`); a word-boundary sweep of `sovereign_mesh::<m>` and
  `crate::<m>` over the workspace gave 0/0 for each. Every remaining shim keeps a
  live importer (`mesh_sim`'s `crate::{tier, oicp_select, throughput_tracking}`,
  the daemon's `sovereign_mesh::worker_eligibility`, the mesh tests'
  `sovereign_mesh::{peer_inference, inference_adapter, decision_*}`). Fixed in
  `a7dc45146`.

Recorded, not changed:

- **`6e7ebd3ed` added `provider_impl.rs` without its `#[path]` declaration** ·
  the commit staged the new file and the registry row (parent re-measured
  5493 → 4692) but not `peer_inference.rs`, so the split was inert: the orphan
  file is not compiled and the parent still measured 5493, which would have
  re-reddened arch-gate. The working tree carried the coherent version, and this
  audit's commit wires it (the `#[path] mod provider_impl;` line lands with the
  builder split). It is recorded because it is the exact failure ARCH 5 names —
  a green LINT/TEST on a change that did nothing.
- **Frozen measurement coordinates** · `quality/DOMAINS.toml`'s `[[noun]]`
  `file` fields, `[[collision]]` `definitions` and the `[[cluster]]` graph
  `cite`/`file` rows still name the mesh paths, and `quality/DOMAINS.md:364`'s
  "the peg `sovereign-serving` carries …" describes the crate at its 2026-09-11
  draft. They are the measurement's own coordinates, not live pointers (the
  audit-3/5/7 precedent); a re-key would falsify the record.
- **`size-gate` (advisory)** · 46 keys grew, the campaign's own growth; the new
  crates read "new and unbaselined". `warn_gate` by design (AGENTS.md), does not
  block PREPUSH. Not re-pinned.

## REVIEW-audit-9 — the AppState dissolution

Range audited: `git log 77f178453..HEAD` (the previous audit's hash) — the six
AppState owner-group moves, the identity reader, the `SelfClaims` port, the
in-flight gauge and the three install-slot replacements. Checks: TESTALL exit=0
(13355 pass, 0 fail); PREPUSH exit=0 (arch-gate cleared below; `size-gate` is
the advisory `warn_gate`).

The row's three claims, verified:

- **No flat owner field remains on `AppStateInner` (63 → 0).** The struct is six
  part fields — `fabric`, `serving`, `node`, `answering`, `ingest`, `workbench`
  (`state.rs:362-395`) — and nothing else.
- **No `install_*`/`with_*` write into a part after construction.**
  `git grep -nE '\.install_[a-z_]+\('` over the workspace finds only unrelated
  installers (recipes, scaffolds, log backends); `OnceLock` is gone from
  `state.rs` and `state/`; and `(app_state|state).inner.(fabric|serving|node|
  answering|ingest|workbench).[a-z_]+ =` matches nothing outside one test
  comment. The readers (`ClockReader`, `PeerTransportReader`, `DialInfoReader`,
  `SlotAliasesReader`, `ServableModelFilesReader`) are created first and
  published through; `LocalInFlightGauge` is created before the provider.
- **Each new port has a planted positive and a planted negative (ARCH 5).**
  `IdentityReader` — `a_published_id_is_read_back` vs
  `a_handle_taken_before_the_swap_observes_it` and `two_readers_do_not_share_a_cell`
  (`sovereign-contracts/src/identity.rs:74,86,97`); `SelfClaims` —
  `claims_round_trip_through_the_trait_object` and
  `record_storage_used_is_observed_by_the_next_claims` vs
  `no_budget_answers_none_not_a_default` and `two_implementations_do_not_share_usage`
  (`sovereign-contracts/src/self_claims.rs:128,140,158,170`); the gauge —
  `clone_shares_the_counter` and `fresh_gauge_reads_zero` vs
  `two_gauges_are_independent` (`sovereign-contracts/src/in_flight.rs:70,81,93`).

Findings, fixed:

- **TESTALL red — `conformance_tags_are_fresh`** · `quality/conformance/
  sovereign-api.toml`, `sovereign-mesh.toml` · `dm-appstate-serving` shifted
  three test lines: `admission.rs` 755→762 and 1077→1084, `routes_status.rs`
  676→684, `daemon.rs` 5285→5291. Regenerated. Fixed in this commit.
- **ARCH 6 (a count that would silently understate) · ARCH 3 (the doc lands with
  the code)** · `quality/DOMAINS.toml` · the range left seven `.rs` files with no
  `[[module]]` row — the six `state/*.rs` parts and `sovereign-daemon/src/lib.rs`
  — so `crate-lines`/`misnamed`/`queue` exited 4 on a coverage hole. Rows added
  (the daemon row carries its own note that the `host` context does not name the
  crate yet, so the home list stays the mint's call). Fixed in this commit.
- **ARCH 3 (the doc lands with the code)** · `quality/DOMAINS.toml` · 42
  `[[module]]` line counts were stale against the tree — 41 of them files this
  range changed (`state.rs` 2486→2146, `daemon.rs` 5613→5619, `ring_sync.rs`
  2031→2072, `gossip.rs` 1493→1478, …), plus `sovereign-serving-host/src/lib.rs`
  31→32 left by audit-8. Re-measured with `wc -l`; the instrument had been
  reporting a stale snapshot. Fixed in this commit.
- **ARCH 3/4 (a pointer to a path that no longer owns the fact)** ·
  `sovereign/SYSTEM_OVERVIEW.md:7366,7388,7421` named
  `AppStateInner.{peer_sched,client_sched,convergence}`; the fields are
  `serving.peer_sched`, `serving.client_sched`, `fabric.convergence` now. `:7388`
  also spelled the client scheduler's key `PrincipalKey` (the wire-side resolver
  key) where the code keys `SchedCore<Principal>` (`state/serving.rs:204-217`).
  `sovereign-api/src/yield_hook.rs:37` named `AppStateInner.corpus_engine`; it is
  `node.corpus_engine`. Repointed. Fixed in this commit.
- **ARCH 3.1 (trim or split) — arch-gate red** · `sovereign-api/src/
  routes_internal/mesh_admin.rs` · the AppState repoint's longer
  `state.inner.<part>` paths made rustfmt reflow the file 1598→1661, +63 past the
  50-line slack. Split the 522-line test module to
  `routes_internal/mesh_admin/tests.rs` via `#[path]` (module path unchanged) and
  the 406-line contribution/activity routes to
  `routes_internal/mesh_admin/contribution.rs`, re-exported at `mesh_admin::*` so
  every importer is unchanged. Parent 1661→740, under the 800 approach-band line
  as well as the slack; arch-gate green. Fixed in this commit.

Recorded, not changed:

- **The shims stay (the row's VERB does not fire).** The range moved fields, not
  modules, so no importer was repointed; every `sovereign_mesh::<m>` shim in
  `sovereign-mesh/src/lib.rs` keeps the live importers audit-8 verified.
- **Frozen measurement coordinates** · `quality/DOMAINS.toml`'s `[[noun]]` `file`
  fields (`state.rs:494`), `[[collision]]` `definitions` and the `[[cluster]]`
  graph `cite`/`file` rows still name the pre-dissolution field paths; they are
  the 2026-09-14 measurement's own coordinates, not live pointers (the
  audit-3/5/7/8 precedent). A re-key would falsify the record.
  `quality/TOPOLOGY.md:56,623` names `commonwealth-api::AppStateInner` in a dated
  census — the peer workspace's old name, also a frozen record.
- **`size-gate` (advisory)** · 49 keys grew, the campaign's own growth; the new
  crates read "new and unbaselined". `warn_gate` by design (AGENTS.md), does not
  block PREPUSH. Not re-pinned.

## REVIEW-audit-principal — the one resolver

Range audited: `git log aff8aad65..HEAD` (the previous audit's hash) — the
`dm-principal-registry` row and `REVIEW-build-principal-wire-fold`,
`-one-resolver`, `-peer-key`, `-attach`. Checks: TESTALL exit=0 (13362 pass, 0
fail); PREPUSH exit=0 (layer-gate cleared below; `size-gate` is the advisory
`warn_gate`; `concept-gate` is the declared could-not-judge).

The row's claims, verified:

- **One resolution produces the published `Principal`; admission's fairness and
  peer keys derive from it (ARCH 8).** One implementation, `AppState::resolve`
  (`sovereign-api/src/principal.rs:165`); its only production callers are
  `client_auth_layer` (`client_auth.rs:209`) and the `AdmissionHost::resolve`
  port (`sovereign-api/src/admission.rs:274`), which the two host middlewares
  reach through `principal_of` (`sovereign-serving-host/src/admission.rs:421`) —
  the attached extension first, the port as the internal router's fallback.
  `peer_sched` is `SchedCore<Principal>` and `peer_tally` is
  `RwLock<HashMap<Principal, PrincipalTally>>`, keyed `Principal::Member
  { node_id }`; `reciprocity_weights` stays `NodeId`-keyed
  (`state/serving.rs:265`), because that is the contribution ledger's key, not
  admission's. The three wire-side types are gone: `git grep 'PrincipalKey\|
  PrincipalSource\|ResolvedPrincipal' -- '*.rs'` finds no code importer (only
  frozen measurement records, below).
- **The five arms are the union, never a narrowing (ARCH 6).** `LocalOwner
  { sub_identity }`, `RemoteClient { credential }`, `Member { node_id }`,
  `Guest { grant }`, `Anonymous` — a superset of the old `PrincipalKey`'s three
  arms (Credential/Declared/Anonymous) and of the old peer gate's node key, and
  the shape DC §3.3 decides.

Findings, fixed (the first two in `e503219bd`, the registry re-key in this
commit):

- **ARCH 8 / `layer-gate` fan-in ratchet** · `sovereign/crates/sovereign-api/
  Cargo.toml:11` · the range added a direct `sovereign-contracts` dependency to
  `sovereign-api`, growing the god-crate fan-in 30 → 31 and reddening
  `layer-gate` (a hard gate). The host's own re-export comment
  (`sovereign-serving-host/src/admission.rs:48-52`) states the intended shape:
  "the daemon reaching the type through the module that publishes it is what
  keeps the direct `sovereign-contracts` fan-in from growing". Repointed the
  two `use sovereign_contracts::principal::Principal;` sites
  (`client_auth.rs:69`, `principal.rs:82`) at
  `sovereign_serving_host::admission::Principal` and dropped the dep. Same type,
  same callers; fan-in back to 30.
- **ARCH 3 (the doc lands with the code)** · `quality/DAEMON_CORE.md:210` ·
  "The full edge resolver below is still `REVIEW-mint-principal`" was falsified
  by `REVIEW-build-principal-one-resolver`; it now names the landed resolver and
  its three callers.
- **ARCH 3 (the doc lands with the code)** · `quality/DOMAINS.toml` · the range
  grew seven `.rs` files without re-measuring their `[[module]]` rows
  (`principal.rs` 375→502, `client_auth.rs` 354→448, `admission.rs` 1112→1121,
  `routes_status.rs` 755→764, `state.rs` 2146→2161, `state/serving.rs` 305→311,
  `sovereign-serving-host/src/admission.rs` 800→922), so `crate-lines`/
  `misnamed`/`queue` read a stale snapshot. Re-measured with `wc -l`; the
  registry is now fresh (0 stale module rows).

Recorded, not changed:

- **ARCH 6 (a precedence interaction, not a missing arm)** ·
  `sovereign-api/src/principal.rs:174-194` and
  `sovereign-serving-host/src/admission.rs:511-522` · the resolver reads the
  bearer before `X-Node-Id`, so a request carrying BOTH resolves to
  `RemoteClient`, and `peer_admission_layer` then records its (valid) `X-Node-Id`
  as malformed and keys the peer ceiling under node zero. The old peer gate read
  `parse_node_id` independent of the bearer. Reachable only for a peer that
  stamps both headers (`peer_inference.rs:3308-3322`, the pinned-pod branch: a
  bearer + `X-Node-Id`). Reordering the resolver is NOT behaviour-preserving —
  the `RemoteClient` arm is what `client_auth_layer`'s token check reads
  (`client_auth.rs:276-282`), so a reorder also changes the auth decision — so
  it needs its own decision row, not an audit fix.
- **Frozen measurement coordinates** · `quality/DOMAINS.md:416` (cites the
  deleted `principal.rs:93,125,147` — the `PrincipalKey`/`PrincipalSource`/
  `ResolvedPrincipal` lines), `quality/DOMAINS.toml:4369` (the cluster note's
  "`Principal` is Admission's decided word", the `dm-principal-registry`
  finding), and `research/scale-analysis/MESH_SCALE_100_USERS_1000_CORPORA.md:
  1503,1510,1645` (`PrincipalKey::Credential`, `SchedCore<PrincipalKey>`). They
  are the measurement's own coordinates at measurement time, not live pointers
  (the audit-3/5/7/8/9 precedent); a re-key would falsify the record.
- **`size-gate` (advisory)** · 49 keys grew, the campaign's own growth; the new
  crates read "new and unbaselined". `warn_gate` by design (AGENTS.md), does not
  block PREPUSH. Not re-pinned.
- **`concept-gate` could-not-judge** · exit 3, declared; the pre-push runner
  counts it as attention, not blocking.

## REVIEW-audit-daemon-1 — the mesh host cluster's move

Range audited: `git log REVIEW-audit-principal..HEAD` — the host-cluster rows
`dm-daemon-mesh-edge`, `-jobs`, `-adapters` and everything they dragged in.
Checks: TESTALL exit=0 (13393 pass, 0 fail); PREPUSH exit=0 (1 of 18 want
attention: `size-gate`, the advisory `warn_gate`). The prior sessions left the
fixes uncommitted; this audit read the tree, ran the two checks, and committed
them at `cafc95dd7` without re-deriving.

The row's claims, verified:

- **`crate-lines --crate sovereign-mesh` drops by the host total.** It reads
  20,990 lines today; the `host` context is absent from `crate-lines`'s output
  and from the registry, and `misnamed` reads sovereign-mesh 13,100/20,990
  (62.4%) fabric. The residue is the two workbench modules (`projects.rs`,
  `reindexer.rs`) and the back-of-house harness, both already minted as leaver
  rows.
- **No mesh module names sovereign-daemon (the `[[forbid]]`'s mirror).**
  `git grep sovereign_daemon -- 'sovereign/crates/sovereign-mesh/src/**/*.rs'`
  is empty.
- **Every dead shim is gone.** `sovereign-mesh/src/lib.rs` dropped
  `landscape_digest_client`, `research_run_dir`, `slot_aliases`,
  `throughput_tracking` and `turn_approval`; each of the sixteen shims left has
  a live importer (`crate::tier`/`crate::oicp_select`/`crate::scheduler_core`
  are the internal ones mesh_sim still reaches).

Findings, fixed (all in `cafc95dd7`):

- **ARCH 3 (the doc lands with the code)** · the move left doc pointers naming
  `sovereign-mesh/src/...`: `quality/conformance-specs.toml` (UI-22, FE-10,
  FE-15, FE-17, FE-43, FE-7), `quality/sabotage/{all,dst,fe-dst,fe-dst-mesh}.toml`,
  `quality/tests/backlog.toml`, `quality/twin-plants.toml:300`,
  `sovereign-api/src/routes_rail.rs:57`, `sovereign-contracts/src/identity.rs:9`,
  `corpus-engine/src/enrichment/state.rs:226`, `commonwealth/docs/*`, `docs/*`.
  Repointed at `sovereign-daemon/src/...`.
- **ARCH 8/9 (one fact, one home)** · `quality/DOMAINS.toml` still tagged
  `sovereign-mesh/src/lib.rs` `host` after the host cluster left and carried
  stale counts for every file the move touched. Retagged host → fabric (the
  crate is Fabric's home now) and re-measured.
- **ARCH 5 (a check with no failing input)** · `code-next-edit/src/lib.rs` had
  no `[[module]]` row, so `crate-lines`/`misnamed`/`queue` exited 4 on a
  coverage hole (`quality/DOMAINS.toml:534`).
- **ARCH 8 (one fact, one home)** · `quality/conformance/sovereign-mesh.toml`
  still carried the UI-22 claim pointing at the moved daemon test. Regenerated:
  the claim lives in the new `quality/conformance/sovereign-daemon.toml:14` and
  is deleted from mesh.
- **ARCH 3.1 (file ceiling)** · `sovereign-mesh/src/ring_sync.rs` (2,072) and
  `oicp-types/src/scoring.rs` (1,269) grew past arch-gate's slack; their test
  modules split out to `ring_sync/{tests,projection_tests,snapshot_tests}.rs`
  and `scoring/tests.rs`, and the DT and egress-census rows re-keyed by path
  with the counts unchanged.
- **ARCH 5 (a check with no failing input)** ·
  `sovereign-mesh/tests/main/replication_sender_census.rs:88` read a split-out
  test module's fake-peer URL as a production sender; excluded `tests.rs` and
  `*_tests.rs` by the convention's filename.
- **ARCH 5/10 (make it structural)** · `scripts/dev-build.sh:104`: `--clean
  --gate-only` leaked `--gate-only` to cargo on the cold path (the loop's CLEAN
  died instead of building), and `du`'s non-zero exit under `pipefail` killed
  the warm path silently before the size gate could decide. Both fixed.
- **ARCH 5 (a gate you have not watched fail)** · the DST soak
  (`dst_scenarios::seeded_chaos_soak`) crossed nextest's 180s wall in a full run
  once `dst.rs` moved into the shared `main` binary; it is slow, not hung, so
  `.config/nextest.toml:128` gives it headroom rather than cutting the seeds.
- **ARCH 6 (never silently substitute)** · `sovereign-mesh/src/projects.rs:5`
  and `reindexer.rs:83` named `sovereign_daemon::supervised_task` in doc
  comments — the forbid's mirror in prose. Rewritten to plain text.
- **ARCH 8 (one clock)** · `sovereign-atos/src/middleware/session_briefing.rs:102`
  read `SystemTime::now()` directly; repointed at
  `sovereign_core::time::unix_now()`.

Recorded, not changed:

- **Frozen `[[cluster]]` rows** · `plan --crate sovereign-mesh` still lists
  `host`, `serving` and `compute` in its move order. They are the registry's
  cluster rows, which predate the 2026-09-14 retags (the `REVIEW-build-dm3-plan`
  finding), not live modules; the plan's `no-new-exception` FAIL on
  `back-of-house` is the same stale row.
- **`size-gate` (advisory)** · keys grew with the campaign; `warn_gate` by
  design. Not re-pinned.


## REVIEW-audit-4 — the vocab door

Range audited: the door's commits (`441e28534..31d68f270`) —
`dm-vocab-compile-fail-test`, `dm-vocab-door-move`, `dm-vocab-atlas-dirname`,
the four `dm-vocab-bypass-*` rows, and `REVIEW-build-vocab-seal`. Checks:
TESTALL exit=0 (13394 pass, 0 fail); PREPUSH exit=0 (1 of 18 want attention:
`size-gate`, the advisory `warn_gate`).

The door, verified against O8's "Done when" clauses that this wave owns:

- **The four readers and the layout constant (O8 checks 1, 10).** `read.rs`
  holds `read_atlas_atoms`/`read_atlas_edges`/`ATLAS_DIRNAME`; the other two
  readers are DEFERRED with the row corrected (`read_atlas_ontology` is
  minted-not-moved and calls tracing; `read_atlas_cross_corpus_edges` needs its
  product types in vocab first — `dm-vocab-door-move`, `ralph/DECISIONS.md`).
- **The seal (O8 check 10).** `AtomsFile` no longer derives `Deserialize`, its
  `atoms` field is `pub(crate)`, `AtomsFileWire` is crate-private and
  `read_atlas_atoms` is the only parser; the trybuild suite flipped `t.pass` →
  `t.compile_fail` with `.stderr` recorded from the SEALED type.
- **corpus-mcp (O8 check 8).** `fn read_atoms` is deleted; `:597` goes through
  the door with the `NotFound` message preserved.
- **The bypasses (O8 check 9).** `git grep -nE
  'from_str::<AtomsFile>|from_slice::<AtomsFile>|: AtomsFile = ' -- '*.rs'
  ':!corpus-engine/'` returns ONE hit, through the door
  (`sovereign-tools/src/atlas_view/atom_browse.rs:258`). The remaining
  `atoms.json` references outside the engine are `exists()`/mtime probes, error
  strings, or the 15 write-path files O8 excludes by name.
- **The leaf (O8 check 11).** `corpus-engine-vocab`'s `[dependencies]` are still
  kernel-types, serde, serde_json, blake3 (trybuild dev-only); `boundary-gate`
  exit 0.

Findings, fixed (all in this commit):

- **TESTALL red — `conformance_tags_are_fresh`** · `quality/conformance/
  sovereign-api.toml:52` was stale: `line = 90` for
  `routes_edit_predictions/outcome.rs` against the generator's 89 (the fn is at
  `outcome.rs:89`). Regenerated with `UPDATE_CONFORMANCE_TAGS=1`; one line.
- **ARCH 3/4 (a citation to a path nobody checked) · docs-gate** ·
  `sovereign/SYSTEM_OVERVIEW.md` cited three paths no file matches:
  `sovereign-api/src/middleware/decision_extractor.rs` and
  `sovereign-tools/src/notes/response_mine.rs` (moved to `corpus-engine-notes`
  by `dm-decision-extractor-move`) and `sovereign-mesh/src/mesh_sim/mod.rs`
  (moved to `sovereign-mesh-test-harness` by `dm-mesh-sim-move`). Repointed at
  the landed homes; the scoreboard test path corrected to `tests/main/`.
- **ARCH 3.2 (a moved oversized file reads as NEW) · arch-gate** ·
  `dm-mesh-sim-move` never re-keyed `quality/baselines/oversized.txt`: the row
  still named `sovereign-mesh/src/mesh_sim/mod.rs`, so the file read as a NEW
  oversized file. Re-keyed the row to
  `sovereign-mesh-test-harness/src/mesh_sim/mod.rs` (§3a step 6, path re-key, no
  debt; the count unchanged, so the move's own +11 rides the 50-line slack).
- **ARCH 3.1 (a file grown past slack) · arch-gate** ·
  `sovereign-api/src/routes_edit_predictions.rs` 1412 → 1474 (+62; the
  `dm-next-edit-move` shell that stayed). Split the wire contract — the four
  caps, the two request shapes and `validate_wire` — to
  `routes_edit_predictions/wire.rs`, re-exported at the historical path;
  1474 → 1357, within slack.
- **ARCH 3.1 (the approach band, a counter ratchet) · arch-gate** · the band
  read 202836 against the 202703 baseline (+133). Cut `sovereign-serving-host/
  src/admission.rs` back under the floor by splitting its test module to
  `admission/tests.rs` (the `ring_sync.rs`/`scoring.rs` pattern
  `REVIEW-audit-daemon-1` used; 922 → 548). Band now 206 files / 201914 lines.
  The director's `REVIEW-audit-daemon-2` ruling ("cut, not banked",
  `2220dbf93`) is satisfied by the same cut; that audit no longer owes it.
- **ARCH 3/4 (the registry does not land with the code)** · `quality/
  DOMAINS.toml` · the two new files had no `[[module]]` row (the coverage
  assertion would exit 4) and the two parents carried stale counts. Added rows
  for `routes_edit_predictions/wire.rs` and `admission/tests.rs`, re-measured
  both parents; `crate-lines`/`misnamed` exit 0.
- **ARCH 3 (a comment that rotted invisibly)** ·
  `corpus-engine-vocab/Cargo.toml:7,18` still called the leaf "behaviour-free …
  and nothing else" after `read.rs` brought the door's one `std::fs` read. The
  description and the carve comment now name it.

Recorded, not changed:

- **The closed-set refusal is the door's designed behaviour, not a regression.**
  `mine_claims` (`sovereign-eval/src/flywheel/mining.rs`) and
  `resolve_cache_is_structural_placeholder`
  (`sovereign-enrichment-build/src/build/steps.rs`) now refuse a whole
  `atoms.json` that carries an atom kind outside the closed set, where their
  `serde_json::Value` walks skipped the unknown atom and kept the rest. That is
  O8's kept seam ("an unknown atom refuses, never skips"); named in
  `dm-vocab-bypass-rest`'s body.
- **`catalog_ingest`'s summary was a pre-existing zero.** `read_atlas_summary`
  treated the `AtomsFile` object as a bare array (`as_array()` is `None` on an
  object), so it reported 0 atoms/edges/themes/questions for every corpus; the
  typed read returns the real counts. Named in `dm-vocab-bypass-tools`.
- **Frozen measurement coordinate** · `quality/DOMAINS.toml:3108`'s
  `[[collision]]` `definitions` line still spells `pub atoms`; it is the
  pre-seal measurement's coordinate, and the same row's `survivor` already
  states the `pub(crate)` shape (the audit-3/5/7/8/9 precedent). A re-key would
  falsify the record.
- **`size-gate` (advisory)** · 65 keys grew, the campaign's own accretion;
  `warn_gate` by design. Not re-pinned.


## REVIEW-audit-daemon-2 — the host crate end to end

Range audited: `git log cafc95dd7..HEAD` for the host-crate rows — the api host
cluster move (`dm-daemon-api-edge`), the cli composition move
(`dm-daemon-cli-composition`), the daemon parts (`REVIEW-build-daemon-parts`,
`-answering-part`, `-embedded-split`, `-fabric-standalone`,
`-membership-lifecycle`) and the read-port leaf (`REVIEW-build-index-read-port`)
— plus the doc and registry drift they left. Checks: TESTALL exit=0 (13397
pass, 0 fail); PREPUSH exit=0 (3 of 18 want attention: `size-gate` and the two
out-of-budget `could-not-judge` instruments; `size-gate` is the advisory
`warn_gate`).

The row's claims, verified:

- **No `host`-tagged module remains in sovereign-mesh or sovereign-api.**
  `sovereign-mesh` holds 0 host rows of 28; `sovereign-api` 0 of 1, after the
  two retags below.
- **`crate-lines --crate sovereign-daemon`.** It reads 78,021 lines over 126
  rows (76,372 host + 1,649 workbench). The instrument itself refuses with the
  coverage-hole exit 4 (below), so the figure was read with
  `coverage_holes = lambda root: []` in a throwaway import — the precedent
  `REVIEW-mint-wave-n` set; the computation is unchanged. `sovereign-mesh` is
  16,684 lines and `sovereign-api` 56 (shim-only), so the api host cluster and
  the composition half are in the daemon.
- **`boundary-gate` green, no new `[[exception]]`.** exit 0; the three
  `sovereign-api` exception rows are unchanged and still live.
- **No shim is deletable in this audit.** Every `sovereign_api::` path has
  ZERO importers outside the crate (`git grep 'sovereign_api::'` over `*.rs`
  excluding the crate is empty), but the crate is shim-only and its three
  `[[exception]]` rows retire with it at `REVIEW-build-sovereign-api-retire`;
  the shim deletion is that row's, not this audit's. The `corpus-engine` index
  shims are `REVIEW-build-ce-kernel-leaf`'s (backlog).

Findings, fixed (all in this commit):

- **ARCH 5 (a check with no failing input) · TESTALL red** ·
  `quality/CONCEPTS.toml:522`'s `EvidenceSet` canonical still named
  `corpus_engine::index::EvidenceSet` after the type moved to `corpus-index`
  (`REVIEW-build-index-read-port`), so `every_register_home_matches_the_working_tree`
  and the positive-control `the_repaired_evidence_chain_canonicals_resolve`
  (`destination.rs:524`) both failed. Repointed both at
  `corpus_index::index::EvidenceSet`.
- **ARCH 5 (a check with no failing input) · TESTALL red** ·
  `chunk_provenance_census::every_acquisition_door_is_a_written_decision` walked
  only `corpus-engine/src`, but `acquired_from_estate`/`acquired_from_peer`
  moved to `corpus-index/src/index/provenance.rs`, so the two declared doors
  read as gone. The scan walks both index crates now.
- **ARCH 3 (a comment citing a path nobody checked) · TESTALL red** ·
  `f26_egress_census::f26_egress_boundary_census`: three rows keyed to moved
  files. The three `ring_sync` test modules moved from
  `sovereign-mesh/src/ring_sync/` to `sovereign-mesh/tests/main/`, outside the
  census's production-`src/` scope — rows removed, not re-keyed (the sites
  still exist, as tests). The three route files moved `sovereign-api` →
  `sovereign-daemon` — rows re-keyed, classes unchanged.
- **ARCH 3/5 (the registry does not land with the code) · TESTALL red** ·
  `conformance_tags_are_fresh`: `quality/conformance/sovereign-api.toml` had no
  `covers:` tags backing it (the crate is shim-only). Regenerated: the manifest
  is deleted and its six claims land in
  `quality/conformance/sovereign-daemon.toml`.
- **ARCH 8 (one fact, one home) · registry drift** · `quality/DOMAINS.toml`
  still tagged `sovereign-mesh/src/fabric.rs` `host` with a stale 315-line
  count and a "scaffolding until REVIEW-mint-daemon-move" note after the file
  moved there at `dm-daemon-api-edge`; retagged `fabric`, re-measured 767.
  `sovereign-api/src/lib.rs` still read `host`; the crate root is a shim and no
  context names the crate, so retagged `unknown` (the `sovereign-grants`
  precedent) with a note.
- **ARCH 3.2 (a moved oversized file reads as NEW) · arch-gate** ·
  `dm-daemon-api-edge` never re-keyed `quality/baselines/oversized.txt`: the
  rows for `corpus-engine/src/index/{mod,search}.rs` still named the old paths,
  so `arch-gate` read `corpus-index/src/index/{mod,search}.rs` as NEW oversized
  files. Re-keyed to the new paths, counts unchanged (§3a step 6, path re-key,
  no debt).
- **ARCH 3.1 (a moved file reads as a new hand-spelled site) · layout-gate** ·
  `corpus-engine/xtask/src/layout_gate.rs:40`'s `DECIDER` still named
  `corpus-engine/src/corpus.rs` after the decider moved to
  `corpus-index/src/corpus.rs`, so the decider file itself read as a NEW layout
  site. Repointed the const and its doc.
- **ARCH 3 (the doc lands with the code) · the composition move's doc drift** ·
  `daemon_cmd/<moved>` still named in live docs and comments:
  `docs/DISTRIBUTED_PILOT_READINESS.md`, `docs/specs/SOLVE_UX.md`,
  `quality/ARCH_LAYERS.toml:73,1316`, `quality/CLEANUP.md:70`,
  `quality/DOMAINS.toml:972,2827`, `quality/campaigns/sv-surface.toml`,
  `quality/sabotage/all.toml:409` (a LIVE mutant target),
  `quality/session-frame.golden.md:139`, `scripts/nc-boundary.py:98`,
  `sovereign/DEFAULTS_LEDGER.md:1810`, and 14 `.rs` doc comments across
  sovereign-cli-daemon, sovereign-contracts, sovereign-daemon, sovereign-desktop,
  sovereign-inference, sovereign-runtime-recipe and sovereign-tools. Repointed
  at `sovereign-daemon/src/...`.
- **ARCH 3 (write for the next reader) · rustfmt** · 17 files carried
  formatting drift from the range (`corpus-engine/src/enrichment/{code_intel,
  entity_extraction,pipeline/runner}.rs`, `types.rs`, `corpus-index/src/filters.rs`,
  `sovereign-core/src/runtime/evidence_loop/anchoring.rs`, `sovereign-tools/src/
  catalog_ingest.rs`, `sovereign-cli-llm/src/enrich_cmd/spec_reconcile.rs`, and
  the tests); `cargo fmt --all` fixed them.

Recorded, not changed:

- **Coverage holes make the domains instrument refuse.** `corpus-index` (19
  files), `sovereign-peer-wire` (1) and `understanding-atlas` (13) have no
  `[[module]]` rows, so `crate-lines`/`misnamed`/`queue` exit 4. Deferred by the
  operator's 2026-09-18 priority cut: `dm-corpus-index-tag` and the
  `dm-understanding-pure-*` rows are in `ralph/BACKLOG-P1-P2.md`, not the
  critical set (`5f81e0452`). The `sovereign-peer-wire` hole is NOT yet named by
  any row: it is 1 file / 20 lines, but which context owns a shared wire leaf
  is a judgment (the same call `dm-corpus-index-tag` makes for retrieval), so it
  is recorded here for a `REVIEW-build-`/`dm-` tag row rather than tagged in
  this audit.
- **`sovereign/HISTORY.md:101,109` and
  `quality/campaigns/handed/order-2-assemble.md:41,71`** keep their
  `daemon_cmd/` spellings: both are records of the state at the time they were
  written, not live pointers. Repointing them would falsify the record.
- **`size-gate` (advisory)** · 69 keys grew, the wave's own accretion and its
  new crates (`sovereign-daemon`, `sovereign-serving-host`, `sovereign-peer-wire`,
  `sovereign-scheduler`, `sovereign-pods`, `sovereign-time`); `warn_gate` by
  design. Not re-pinned.

## REVIEW-audit-wave-2 — sovereign-api gone, and the shims its moves left dead

Range audited: `git log c9cff4dd6..HEAD` — the `sovereign-api` retire
(`REVIEW-build-sovereign-api-retire`, `78e38ce76`) and the campaign amendment
that landed beside it. Checks: TESTALL exit=0 (13397 pass, 0 fail); PREPUSH
exit=0 (2 of 18 want attention: `concept-gate` NEVER-RAN because
`sovereign-cli-dev` is not built — the same abstention the prior audit recorded
as `instrument-gate` — and `size-gate`, the advisory `warn_gate`, 69 keys
grown and not re-pinned).

The row's claims, verified:

- **sovereign-api is gone.** `ls sovereign/crates/sovereign-api` -> No such
  file; no `sovereign-api` member or `[workspace.dependencies]` line in the
  root `Cargo.toml`; `git grep 'sovereign_api::' -- '*.rs'` is 0.
- **Its three `[[exception]]` rows are zero.** `grep 'from = "sovereign-api"'
  quality/ARCH_LAYERS.toml` is empty; the `sovereign-api -> sovereign-*` and
  `sovereign-scheduler -> sovereign-api` forbids and the `mesh-api` layer entry
  went with them; `layer-gate` passes with no STALE verdict.
- **`plan --crate sovereign-api` reads no cluster** (CENSUS 11/11 axes);
  `crate-lines --crate sovereign-api` still exits 4 on the pre-existing
  coverage hole (`corpus-index`, `understanding-atlas`, `sovereign-peer-wire`),
  reported not defaulted — the crate itself has no `[[module]]` rows.
- **The two live instruments the retire repointed work.**
  `scripts/daemon-route-census.py` -> 302 registrations / 284 unique paths
  (sovereign-daemon=275); the tracing filters read `sovereign_daemon=info`, so
  the moved modules' logs do not go dark.

Findings, fixed (commit `c765d09ec`):

- **ARCH 3 (a shim whose importers are all repointed) · dead re-export ·
  `sovereign/crates/sovereign-mesh/src/lib.rs:60,64,86`** — three shims created
  by `REVIEW-build-sched-move` and `dm-mesh-workbench-move-scip` (`pub use
  corpus_engine_scip::lsp_tier`, `pub(crate) use sovereign_scheduler::oicp_select`,
  `pub(crate) use sovereign_scheduler::scheduler_core`) had zero importers after
  their only consumers moved to `sovereign-serving-host`/`sovereign-scheduler`. A
  repo-wide `git grep` finds no `crate::<m>`/`sovereign_mesh::<m>` site outside
  the shim and the owner crate, so they are deleted. The remaining `// shim:`
  re-exports all have live importers (mesh tests, the daemon bootstrap,
  `cli-llm`), and the `corpus-engine` leaf shims re-export whole `pub use` globs
  whose symbols are still used, so neither set is deletable.

Recorded, not changed:

- **The coverage holes the domains instrument refuses on are unchanged.**
  `corpus-index` (19 files), `understanding-atlas` (13) and
  `sovereign-peer-wire` (1) still have no `[[module]]` rows; deferred by the
  2026-09-18 priority cut and recorded by `REVIEW-audit-daemon-2`.
- **`quality/CONCEPTS.toml:961`'s `Principal` canonical does not resolve.** The
  retire repointed it from `sovereign_api::principal::Principal` (a crate now
  deleted) to `sovereign_daemon::client_principal::Principal`, but the type is
  defined at `sovereign_contracts/src/principal.rs:59` and
  `client_principal.rs:82` only `use`s it (private), so the path is not
  reachable. The register tolerates it because the row's `home = "planned"` and
  `every_register_home_matches_the_working_tree`
  (`sovereign-cli-dev/src/refactor_cmd/destination.rs:602`) only fails a
  *minted* row whose canonical stops resolving. The correct planned home is a
  register-owner judgment (the `measure` says "may move beside `Scope`"), so it
  is recorded, not guessed.
- **`quality/CONCEPTS.toml:968`'s `measure` still cites
  `sovereign-api/src/principal.rs`** — a frozen measurement string, left as
  written (the retire updated the live `canonical` beside it).
- **The design docs still describe sovereign-api in the present tense.**
  `quality/DAEMON_CORE.md:293` ("Today the node is assembled three times over …
  `sovereign-api`'s `AppState` carries the rest"), `quality/DOMAINS.md:375,416`,
  and `corpus-engine/DECOMPOSITION.md:363` ("reads products … `sovereign-api` 7").
  These are design records of the pre-move state — the same class the prior
  audit left in `sovereign/HISTORY.md` and the handed orders — so they are
  recorded, not rewritten.
- **The frozen `[[noun]]` rows keyed `crate = "sovereign-api"`
  (`quality/DOMAINS.toml:1866` onward) stay**, per the retire's DECISIONS
  entry; their `file` coordinates already point at `sovereign-daemon`.
- **`size-gate` (advisory)** · 69 keys grew — the wave's accretion and its new
  crates; `warn_gate` by design. Not re-pinned.

## REVIEW-audit-mesh-rest — wave 1's close on sovereign-mesh

Range audited: `git log c765d09ec..HEAD` (the previous audit's hash) — the
`sovereign-api` retire's follow-ons: `REVIEW-build-code-facts` (the `code-facts`
crate), `REVIEW-build-mesh-workbench-deferred` (the last two mesh modules to
`corpus-engine-watchers`), `dm-registry-coverage` and the `DEMO-d5-misnamed`
close. Checks: TESTALL exit=0 (13397 pass, 0 fail); PREPUSH exit=0 (2 of 18 want
attention: `concept-gate` could-not-judge, declared; `size-gate`, the advisory
`warn_gate`).

The row's claims, verified:

- **No non-fabric `[[module]]` remains under `sovereign-mesh/src`.** `misnamed`
  reads sovereign-mesh 13,855 / 13,855 = 100.0% fabric with no MISNAMED flag;
  all 18 rows are `fabric`.
- **`crate-lines --crate sovereign-mesh` reads <= 20,000.** It reads 13,855.
- **`plan --crate sovereign-mesh`'s problems are the registry's frozen
  `[[cluster]]` rows.** All nine trace to the 2026-09-14 measurement (banner
  `quality/DOMAINS.toml:4432,4437-4446`, "115 files, 88,255 lines"): five "dest
  not in context crates" from the prose `dest` strings (`workspace`, `retrieval`,
  `workbench`, `ingest`, `understanding`) and four `no-new-exception` fails from
  the same rows' `own_deps`/`imports_clusters` (`back-of-house`, `serving`,
  `workbench`, `fabric`). None is a live edge — the fabric row's
  `sovereign-mesh -> sovereign-daemon` is the `[[cluster]]` graph's own
  `imports_clusters`, not a production dep (`sovereign-daemon` is
  `[dev-dependencies]`, `sovereign-mesh/Cargo.toml:328`).

Findings, fixed (commit `8b0390a14`):

- **ARCH 3 (a shim whose importers are all repointed)** ·
  `sovereign/crates/sovereign-mesh/src/lib.rs:31,86` · two dead shims deleted:
  `corpus_engine_watchers::commit_harvest` — its only reader, `reindexer`, moved
  to `corpus-engine-watchers` and reaches `crate::commit_harvest` within its new
  crate — and `sovereign_scheduler::tier` — its last reader, the Tier-1
  simulator, moved to `sovereign-mesh-test-harness` and names
  `sovereign_scheduler::tier` directly. A word-boundary sweep of
  `sovereign_mesh::<m>` and `crate::<m>` over the workspace gave 0/0 for each
  (the only remaining hits are Cargo.toml comments). The two comments
  (`Cargo.toml:113-116`, `:150-152`) are rewritten to name the real importers.
- **ARCH 3 (the registry does not land with the code)** · `quality/DOMAINS.toml`
  · four `[[module]]` line counts stale against the tree:
  `sovereign-mesh/src/lib.rs` 98→93 (this audit's deletion),
  `corpus-engine-watchers/src/reindexer.rs` 2150→2155,
  `corpus-engine-watchers/src/lib.rs` 51→75,
  `sovereign-cli-dev/src/code_cmd.rs` 1847→1846.
- **ARCH 3.2 (a moved file reads as NEW) · arch-gate** ·
  `quality/baselines/oversized.txt:154` · `REVIEW-build-mesh-workbench-deferred`
  moved `reindexer.rs` without re-keying its oversized row; re-keyed
  `sovereign/crates/sovereign-mesh/src/reindexer.rs` →
  `corpus-engine-watchers/src/reindexer.rs`, count unchanged (§3a step 6).
- **ARCH 3.2 (a moved file reads as NEW) · clock-gate** ·
  `quality/baselines/clock_reads.txt:73` · the same move left the clock row keyed
  to the old path; re-keyed to `corpus-engine-watchers/src/projects.rs`, count 3
  unchanged (the moved file still has three `SystemTime::now()` reads).
- **ARCH 3/4 (path-keyed registries)** · `quality/sabotage/all.toml:59`,
  `quality/sabotage/gr.toml:359,369` · three live mutant targets still keyed
  `sovereign/crates/sovereign-mesh/src/reindexer.rs`; re-keyed to
  `corpus-engine-watchers/src/reindexer.rs` (the `find` body still matches the
  moved file at `:471`), or the mutants can never be planted.
- **ARCH 3/4 (a doc naming a path that no longer owns the fact)** · the two
  moves left live pointers, repointed: `corpus-engine-scip/src/error.rs:13`,
  `sovereign/crates/sovereign-contracts/src/rebrand.rs:313`,
  `sovereign/crates/sovereign-tools/src/code/symbol_lookup.rs:270`,
  `sovereign/scripts/cli-journey-sandbox.sh:364` (line also corrected 386→457),
  `sovereign/SYSTEM_OVERVIEW.md:8659`, `quality/session-frame.golden.md:135-137`
  (the `facts`/`facts_store` pair to `code-facts/src/`),
  `sovereign/crates/sovereign-cli-dev/src/intent.rs:609` (the `crate_of` example
  → `code-facts/src/facts_check.rs` / `"code-facts"`), and
  `sovereign/crates/sovereign-cli-dev/src/refactor_wire.rs:108`.

Recorded, not changed:

- **Frozen measurement coordinates** · `quality/DOMAINS.toml`'s `[[cluster]]`
  rows and `[[noun]]` `file` fields still name the 2026-09-14 mesh paths (the
  banner at `:4432`), and `sovereign/DEFAULTS_LEDGER.md:536`,
  `sovereign/docs/archive/PHASE_7_GAP_CLOSURE_PLAN.md:94`,
  `sovereign/handoff/CODE_INTEL_DEMO.md:178,420` and
  `quality/campaigns/handed/order-3-writers.md:168` are records of their moment.
  A re-key would falsify the record (the audit-3/5/7/8/9 precedent).
- **`corpus-engine/xtask/src/clock_gate.rs:270`** · `each_island_is_pointed_at_its_own_decider`
  uses `corpus-engine/src/facts_store.rs` as its corpus-engine example after the
  file moved to `code-facts`. The assertion is a pure path-prefix test that still
  passes, so the example is historical, not a broken gate; a re-pointed example
  would take `decider_for`'s else branch (it prefixes on `corpus-engine`, not
  `code-facts`), which is a `decider_for` decision, not this audit's.
- **`scripts/tests/lint-could-not-judge.sh:87`** · `corpus-engine/src/facts.rs:12:5`
  is a synthetic diagnostic fixture, not a pointer.
- **`sovereign/SYSTEM_OVERVIEW.md:4183`** names `sovereign-mesh::reindexer` —
  the shim, which still resolves and is the path the daemon reaches; the
  historical-path re-export is intentional (§3a step 5), not drift.
- **`size-gate` (advisory)** · 69 keys grew, the campaign's accretion;
  `warn_gate` by design. Not re-pinned.
- **`concept-gate` could-not-judge** · exit 4, declared; not blocking.
