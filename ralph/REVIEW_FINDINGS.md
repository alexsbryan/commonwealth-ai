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
  `_PEER_WORD`. Fixed in the REVIEW-audit-1 commit.
- **ARCH 8 (one accessor per path)** · `scripts/domains-census.py:228` ·
  `peer_defs` read the repo registry via `registry()` while every sibling axis
  (`shared_edges`, `word_owners`, `atom_defs`, `misnamed`, `queue`, `liftable`,
  `predicate_problems`, `plan`) reads `registry_for(root)`; a fixture-planted
  `quality/DOMAINS.toml` would be ignored by this axis alone. Fixed: use
  `registry_for(root)`. Behaviour on the tree and on every existing fixture is
  unchanged (no peer fixture plants a registry). Fixed in the REVIEW-audit-1
  commit.

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

## Pre-existing red gates (not rung 3; see ralph/NEEDS_HUMAN.md)

`TESTALL` and `PREPUSH` are red on this tree for reasons that predate rung 3
and are already public on `origin/main`; none is touched by
`origin/main..HEAD`. Evidence and the operator decision are in
`ralph/NEEDS_HUMAN.md`.
