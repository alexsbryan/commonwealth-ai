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
