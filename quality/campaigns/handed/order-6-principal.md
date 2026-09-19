---
schema: work-order/v1
id: handed-6-principal
status: open
drafted: 2026-09-17
approved: pending
serves: handed
campaign: handed
lane: structural — principal: the guest decider is already exhaustive; this rung watches it refuse
engine: ralph pool; one REVIEW-DEMO row in the main workdir. No build row, no HUMAN row
budget: 1 row, 0 files changed, 0 commits of source; covered by the hd-6 REVIEW-audit
---

# Order: handed-6-principal — watch the exhaustive Scope decider break, then revert

## Objective

`Scope` is already a closed enum whose every decider matches exhaustively with no `_` arm, so a new
variant cannot be added without the build breaking at each place that would otherwise silently
ignore it. That is the ability the campaign names, and it is already true in the code. What is NOT
true is that anyone has ever watched it fail: principle 5 — a gate you have not watched fail is not
a gate — so this rung is the plant, not a change. One row plants `Scope::Audience(String)`, pastes
the compiler's refusal, reverts, and leaves LINT green. Nothing is committed but the row mark.

Cut at round 1 (2026-09-17), 4 rows to 1: no `Limits` type is minted (`Limits` pays off only for
precondition kinds, and warrant wr-2 — which would supply them — is parked; the exhaustive matches
already give the same E0004), `hd-6-mint-refuses-empty-models` and `hd-6-scope-mentions` are dropped
with it, and the HUMAN row is moot because no behaviour changes.

## Premises (verified 2026-09-17, file:line)

- `pub enum Scope { Models(Vec<String>), Rails(String) }` —
  sovereign/crates/sovereign-grants/src/guest_grant.rs:76 (variants :80, :88). The doc at :71-74
  already tells a future author what adding a variant costs.
- `grep -n '_ =>' sovereign/crates/sovereign-grants/src/guest_grant.rs` returns **nothing**. Every
  match on a `Scope` in the workspace names both variants:
  - `Scope::paths` — `match self` at guest_grant.rs:103, arms :104, :105.
  - `Scope::label` — `match self` at :111, arms :112, :113.
  - `GuestGrant::models` — `find_map(|s| match s` at :160, arms :161, :162.
  - `GuestGrant::rail_namespace` — at :175, arms :176, :177.
  - `GuestGrant::summary` — `.map(|s| match s` at :196, arms :197, :198.
  - `into_scopes`' dispatchable check — `let ids = match scope` at
    sovereign-api/src/routes_internal/guest_grant.rs:135, arms :136, :142.
  - the test ratchet — `match s` at sovereign-api/tests/main/client_auth.rs:446, arms :447, :448,
    under a doc comment (:437-439) that says in English exactly what the plant proves: "adding a
    variant makes it non-exhaustive, so the build breaks HERE".
  That is **14** arm lines across **seven** match sites, not two.
  `git grep -n 'Scope::Models(_)\|Scope::Rails(_)' -- '*.rs'` prints **9** lines — 9 of those 14 arms.
  The other five bind a name and the grep cannot see them: `Scope::Models(ids)` at
  routes_internal/guest_grant.rs:136, guest_grant.rs:161 and :197; `Scope::Rails(ns)` at
  guest_grant.rs:176 and :198. So the grep is a floor on the arms, not a census of them; the
  enumeration above is the census.
- `permits_path` (guest_grant.rs:151) is the only decider the auth layer calls
  (sovereign-api client_auth.rs:253) and it consults `paths()` of every scope, so a variant with no
  `paths()` arm cannot reach the allowlist by accident — it cannot compile.
- `every_scope_paths_are_mounted_and_never_privileged` (client_auth.rs:462) drives
  `one_sample_per_scope_variant()` and asserts each path is mounted and matches no `NEVER_GUESTABLE`
  prefix (`&["/internal/", "/v1/apps", "/v1/mesh/"]`, :433).
- `sovereign-api` depends on `sovereign-grants` (sovereign-api/Cargo.toml:20), and
  `tests/main/client_auth.rs` is part of the `main` integration-test target
  (sovereign/crates/sovereign-api/tests/{main.rs, main/, rail_e2e/}), which `sovereign-lint.sh`
  compiles because it runs `--all-targets`.
- **Two passes are required, and this is the one thing the round-1 design did not price.** A single
  plant cannot paste both cited sites: if `Scope::Audience(String)` is added with no arms,
  `sovereign-grants` itself fails to compile, so `sovereign-api` is never checked and
  `client_auth.rs:446` never reports. The row therefore runs the plant twice — once bare (the
  in-crate deciders refuse) and once with the five grants arms filled in (the downstream ratchet and
  the API dispatcher refuse). handed.toml's `enforced_by` names both sites, so both are owed.
- `sovereign-cli-llm/src/chat_cmd/bootstrap.rs:592` mentions `Scope` in prose only; no row edits it
  (the `hd-6-scope-mentions` row that would have is cut).

## Steps

1. Plant, paste, revert — `REVIEW-DEMO-hd-6-scope-plant`. There is no step 2, and no source commit.

## Seams

- **Nothing is changed.** No file in the tree differs after this row. `git status --short` must be
  as clean at the end as at the start (aside from `ralph/STATE.md`).
- Do NOT mint `Limits`, do NOT touch `GuestGrantStore::issue`, the mint wire shape
  `{scopes:{models?,rail?}, ttl_secs?, label?}`, `/mcp` (already loopback-sealed by
  `mcp_router.rs:198`; no tunnel reaches that listener — iroh_access.rs:440-465), `ingest_grant`,
  `EphemeralIngestGrant`, or commonwealth-core.
- The domains pool holds uncommitted edits in `sovereign-api/src/routes_rail.rs` and
  `sovereign-cli-llm/src/mesh_guest.rs`. Neither is edited here, but the row runs LINT in the MAIN
  tree, so a red from a peer's in-flight edit must be distinguished from the plant's red before the
  paste — run LINT once BEFORE planting and paste that baseline too.
- hd-3 touches `sovereign-api/src/auto_recover.rs`; no overlap.

## Done when

- The commit body (a `ralph/STATE.md`-only commit) carries four pastes: the pre-plant LINT
  `exit=0`; pass 1's E0004 lines at guest_grant.rs:103, :111, :160, :175, :196; pass 2's E0004 lines
  at routes_internal/guest_grant.rs:135 and tests/main/client_auth.rs:446; and the post-revert LINT
  `exit=0`.
- `git status --short` shows no source file changed.
- The audit re-runs pass 1 and checks the code is **E0004** ("non-exhaustive patterns"), not merely
  that something was red.

The promise this rung makes and everything it does not cover live in one record — principle 8:
`quality/campaigns/handed.toml` `[[ability]] principal`, fields `promise` and `not_covered`.

## Kill

- Pass 1 comes back green — the enum is not the closed set this order claims, or a `_` arm was added
  between this order and the run. Stop and say so: the ability is not structural and the campaign's
  `today` line ("already true in the code and never watched failing") is wrong.
- Pass 2 cannot be made to fail because `sovereign-api`'s test target is out of the scoped LINT's
  reach even under `--full`. Stop: the ratchet at client_auth.rs:446 is not reachable by any gate
  the loop runs, and naming it in `enforced_by` is a claim with no instrument.
