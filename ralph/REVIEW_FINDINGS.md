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
