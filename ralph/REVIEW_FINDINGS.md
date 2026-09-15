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
