---
schema: work-order/v1
id: handed-0-adopt
status: open
drafted: 2026-09-17
approved: pending
serves: handed
campaign: handed
lane: bookkeeping — no cargo; a REVIEW- row so it runs in the main workdir, where the gitignored orders live
engine: any
budget: 1 unit; no build
---

# Order: handed-0-adopt — every active campaign is closed into handed

## Objective

The operator adopted every active campaign into `handed` on 2026-09-17. The
per-row dispositions are recorded in `quality/campaigns/handed.toml`
`[[adopted]]` and in `.sovereign/features/handed/campaign.md` Decisions. This
order makes the adoption true on disk: thirteen campaign files say `closed`,
and the orders that served them close, so the boot block and
`scripts/co-lineage.py list` stop reporting work nobody will run.

## Premises

- Thirteen files in `quality/campaigns/` carry `status    = "active"`:
  conformance, deletion, domains, drb1-race, epistemic-index,
  financial-corpora, hot-path-reuse, house-mesh, refactor-factory, ring-apps,
  sv-surface, verifier-loop, warrant (`grep -l 'status *= *"active"'`).
- 42 files cite those paths, so the files stay where they are; moving them to
  `quality/campaigns/closed/` would be a citation edit with no bearing on the
  objective.
- `scripts/co-lineage.py` accepts `active | staged | closed` (03155d6c6).
- 48 orders under `.sovereign/features/` read `status: open|approved`
  (gitignored, per-host); `scripts/co-order.sh close <id> abandoned` closes one. These files are gitignored, so this step is only correct in the MAIN workdir — a pool lane's worktree has no `.sovereign/`.

## Steps

1. In each of the thirteen files, replace the `status` line with
   `status    = "closed"` followed by
   `# closed 2026-09-17: adopted by handed — dispositions in quality/campaigns/handed.toml [[adopted]]`.
   Row `REVIEW-build-hd-0-adopt`.
2. For every open order whose `campaign:`/`serves:` names one of the thirteen,
   noun-convergence, cw-lift, recursion-as-workflow, or is unattributed, run
   `scripts/co-order.sh close <id> abandoned` — EXCEPT `handed-*` orders. Same
   row; these files are gitignored, so nothing to commit for this step.

## Seams

Touches only the thirteen `status` lines and gitignored order files. Must not
touch `quality/campaigns/handed.toml` or any `closed/` file.

## Done when

`python3 scripts/co-lineage.py list` prints `handed` as the only non-closed
campaign, and `scripts/co-order.sh list` prints only `handed-*` orders.

## Kill

None — bookkeeping. If `co-lineage.py list` errors after step 1, revert and §6.
