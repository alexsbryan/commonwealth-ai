---
name: fieldglass
description: Run the Fieldglass morning-glance ritual — render the architecture-health page (svrn code fieldglass) and relay what changed since the last glance. Evidence, never verdicts.
---

The glance ritual for `docs/FIELDGLASS.md`. The page is the instrument and the
operator's eye is the judge: this skill runs the render, opens the page, and
relays only what MOVED since the last glance. It never scores, never gates, and
never summarizes "health" — that boundary is standing (FIELDGLASS.md, "What it
is not").

## Steps

1. Render from the repo root (~2-4 min; the duplication NEAR tier embeds —
   progress lines follow):

   ```
   svrn code fieldglass --open
   ```

   (`svrn` is the prod symlink. Some dev hosts carry only the legacy
   `sovereign` symlink — substitute it if `svrn` isn't on PATH.)

   Quick pass: `--no-dup`. Layer skips (`--no-git`, `--no-agent`) are allowed
   and self-reported in the page footer. Output lands at
   `~/.sovereign/arch/<corpus>/fieldglass.{html,json}`.

2. Extract the relay fields from the JSON sidecar with `jq`/python — do NOT
   Read the whole file (its `files` array holds every `.rs` leaf):

   - `.delta` — `grown` (path, line delta), `new_offenders`, `new_files`,
     `removed_files`, `prev_unix`. A `null` delta means FIRST RENDER — say
     "first render, no baseline", never "no change" (§18.2).
   - `.honesty.scip_commits_behind` — >0 means the structural panels describe
     an older commit than HEAD; name the number and the refresh
     (`svrn code index . --corpus-id=<corpus>`).
   - `.honesty.chunk_index_age_days` — the NEAR-duplication tier's skew (the
     exact tier is as fresh as `scip_head`; the two ride different cadences).
   - `.honesty.notes[]` — render decisions (skipped panels, arc-cap drops);
     relay any the operator didn't opt into with a flag.
   - `.attention.tollbooths` / `.comprehension_tax` / `.offenders` — headline
     only entries that are NEW or moved per the delta; the standing lists live
     on the page.

3. Relay in the operator's terms, deltas first: "since your last glance:
   `runtime.rs` +212 lines; one new >1200 offender; inputs N commits behind."
   Point at the open page for the shapes — the relay carries what changed, the
   page carries the evidence. No scores, no pass/fail, no "looks healthy".

4. If the glance surfaced something durable — a new tollbooth, a first-time
   offender, stale inputs recurring across glances — record it with `note`
   (kind `decision` or `todo`) so the next glance inherits it.

## Caveats

- **A FULL render replaces its own baseline.** `compute_delta` diffs against
  the sidecar the full render then overwrites, so running it twice
  back-to-back reports an empty delta the second time — don't re-run to
  double-check; the first render's relay is the one that counts. Degraded
  renders (any `--no-*` flag) never touch the baseline, so a quick pass is
  always safe to run between glances.
- **No cron.** Recorded house decision (same as fleet-report): scheduled runs
  only after the operator has reviewed manual renders for about a week and
  asks. Until then this skill is invoked by hand.
- The agent-heat overlay counts only `file_path`-carrying tool calls across
  local transcripts — hook-injected context structurally can't pollute it, but
  transcripts from other machines are invisible; `honesty.agent_sessions`
  states the coverage.
