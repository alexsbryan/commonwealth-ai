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
- **The open-order census, measured 2026-09-17.** 55 orders under `.sovereign/features/`
  read `status: open|approved` — 52 `open` plus 3 `approved` (`nc-2c-backflow-tail`,
  `nc-6-atom`, `nc-7-note`); `list` filters on `status = "open"` exactly
  (co-order.sh:193), so it prints those 52 (plus two mesh shadows, next premise).
  Resolving every one's `serves:`/`campaign:` frontmatter, **49 of the 52 are in step 2's
  verb**: 13 serve one of the thirteen (refactor-factory 3, ring-apps 3, warrant 3,
  conformance 2, domains 1, financial-corpora 1) · 16 serve `cw-lift` (already closed —
  `quality/campaigns/closed/cw-lift.toml`) · 3 serve `noun-convergence`
  (`status = "closed"`) · 1 serves `recursion-as-workflow` (`rec-1-explicit-stack`; its
  `campaign: recursion` has no campaign file) · 10 are unattributed (`serves:` absent or
  `(unattributed…)`, 6 of them under a `campaign: quality-surface` that has no file
  either) · 6 are the stale `handed-*` dirs. The **other 3** name a campaign this order
  does not adopt and are not unattributed, so no verb here reaches them:
  `rel-1-release-path` (`campaign: release-hygiene`), `tn-1-terminal-honesty` and
  `tn-2-one-decider-per-turn` (`campaign: (none)`, prose `serves:`).
- `scripts/co-order.sh close <id> abandoned` closes one and needs a LOCAL file
  (`[ -e "$F" ] || … exit 2`, co-order.sh:407). Two rows `list` prints have none and so
  cannot be closed by this step: `--help` (a mesh note shadow stamped to this machine)
  and `ring-doc-week1` (peer RuggedFox). The order files are gitignored, so this step is
  only correct in the MAIN workdir — a pool lane's worktree has no `.sovereign/`.

## Steps

1. In each of the thirteen files, replace the `status` line with
   `status    = "closed"` followed by
   `# closed 2026-09-17: adopted by handed — dispositions in quality/campaigns/handed.toml [[adopted]]`.
   The same row flips `quality/campaigns/handed.toml`'s own `status = "staged"` (:11) to
   `status = "active"`: `scripts/co-lineage.py:83` defines staged as "listed, never measured" and
   `measure --all-active` filters on `status == "active"` (:1485), so a campaign left staged is
   never measured. Row `REVIEW-build-hd-0-adopt`.
2. For every open order whose `campaign:`/`serves:` names one of the thirteen,
   noun-convergence, cw-lift, recursion-as-workflow, or is unattributed, run
   `scripts/co-order.sh close <id> abandoned` — `handed-*` orders INCLUDED. Then
   remove the six superseded per-order directories, which exist on this host and
   are all `status: open`: `.sovereign/features/handed-{1-render,3-writers,4-ledger,5-custody,6-principal,7-surfaces}`.
   They hold the ROUND-1 designs round 1 cut — the lease ("a lease fronts every
   index write"), the `SharingPolicy` type ("one SharingPolicy, default private"),
   four surfaces ("four surfaces, two turns each") — and `scripts/co-order.sh list`
   prints those titles from each file's `# Order:` line (co-order.sh:190-196), so a
   worker or director reaching for "the order" gets the rejected design. Keep ONLY
   `.sovereign/features/handed/`, which holds `campaign.md`, not an order. The
   surviving orders are `quality/campaigns/handed/order-<n>-*.md`. Same row; these
   files are gitignored, so nothing to commit for this step.

## Seams

Touches the thirteen `status` lines, gitignored order files, and `handed.toml`'s
own `status` line (:11) — that one line is the single exception, taken in step 1
because nothing else measures the campaign. Must not touch any OTHER line of
`quality/campaigns/handed.toml` (the `[[adopted]]`, `[[ability]]`, `[[rung]]` and
`[[bar]]` tables are other rows' work) or any `closed/` file.

## Done when

`python3 scripts/co-lineage.py list` prints `handed` as the only non-closed campaign and
`handed` reads `status = "active"`.

`bash scripts/co-order.sh list` is then EMPTY OF STEP 2's SCOPE — no printed row whose
`serves:`/`campaign:` names one of the thirteen, `handed`, `cw-lift`, `noun-convergence`
or `recursion`/`recursion-as-workflow`, and none whose `serves:` is absent or begins
`(unattributed`. `list` prints only id + title (co-order.sh:195), so the predicate is
checked against the frontmatter, not the output — one loop, and it must print nothing:

```sh
bash scripts/co-order.sh list | awk '{print $1}' | while read -r id; do
  f=".sovereign/features/$id/order.md"; [ -e "$f" ] || continue
  v="$(sed -n 's/^serves: *//p;s/^campaign: *//p' "$f")"
  case "$v" in ""|"(unattributed"*|*handed*|*cw-lift*|*noun-convergence*|*recursion*|\
*conformance*|*deletion*|*domains*|*drb1-race*|*epistemic-index*|*financial-corpora*|\
*hot-path-reuse*|*house-mesh*|*refactor-factory*|*ring-apps*|*sv-surface*|*verifier-loop*|\
*warrant*) echo "STILL IN SCOPE: $id" ;; esac
done
```

**49 of today's 52 close** (`co-order.sh close … abandoned`, `handed-*` included), and the
6 `handed-*` directories go after that. A residual row is NOT a failure; today there are
five, and they are the predicate's complement: `rel-1-release-path`,
`tn-1-terminal-honesty` and `tn-2-one-decider-per-turn` serve a campaign this order does
not adopt, and `--help` and `ring-doc-week1` are mesh shadows with no local file, which
`close` refuses by construction (co-order.sh:407).

## Kill

None — bookkeeping. If `co-lineage.py list` errors after step 1, revert and §6.
