---
schema: session-frame/v1
session_id: 2fa2ddbb-eaf1-4df9-ac42-4990bf2a7701
harness: claude-code
model: claude-fable-5
repo: commonwealth-ai
branch: main
head_at_end: 323a5520
started_at: 2026-07-24T04:21:03Z
ended_at: 2026-07-24T05:20:56Z
status: completed
provenance: hand-written
notes: []
---

<!--
E4a golden. Hand-authored 2026-07-24 from the transcript SPINE
(~/.sovereign/sessions/2fa2ddbb/spine.txt: 5 user turns, 25 assistant
texts, 10 edited files) — NOT from the session's self-reported frame,
so grading the self-reported frame against this measures the
session_state encode-time write path independently (MEMORY_MODEL E4a
gate). Load-bearing items only; Next/Invariants weighted x2 per spec §5.
-->

## Goal

Continue the memory-model initiative as successor of split experiment #2
(donor f7054fcd): ship item #2, the weekly fleet report. Then two
user-raised threads: (u3/u4) make the continuity protocol actually work on
teammates' machines given they share a commonwealth mesh, and (u5) find why
SCIP goes stale despite a morning refresh + a full day of hook activity.

## State

Four commits, suite-green, all live-verified:

- `9941d71e` — `scripts/fleet-report.py` + `/fleet-report` skill. Stdlib-only
  weekly report composing existing surfaces (cache-audit `--json` table, the
  `--ramp` / `--counterfactual` outputs parsed as text, split-events.jsonl,
  frame frontmatter, a transcript commit scan). Writes
  `~/.sovereign/reports/fleet-<date>.md` + a JSON sidecar that drives the
  next run's trend column.
- `73068537` — distill provenance guard: `session distill` skips the frame
  write when `frame.md` is `provenance: self-reported` (spine still
  refreshed; `--force` overrides). Bundled fix: `--project .` canonicalizes
  instead of encoding as the literal dir `-`.
- `b01e302b` — all six hook commands anchored to `$CLAUDE_PROJECT_DIR` so a
  drifted shell cwd no longer breaks them.
- `323a5520` — continuity preflight gate + mesh peer verification in
  `agent-preflight.py`. Gate run: 12 pass / 0 fail local; LittleMac WARN
  (asleep).

## Next

1. SCIP staleness — ROOT CAUSE DIAGNOSED, fix NOT built: four registered
   projects are NESTED (commonwealth-ai root contains sovereign,
   corpus-engine, commonwealth as separate projects); every save dirties 2+
   projects, each runs its own rust-analyzer export, all four observed
   `[rebuilding]` at once, commonwealth "never built", the queue never drains
   on a heavy day. Contends with the lint/test watcher + inference. Fix
   directions cheapest-first: (a) deregister nested projects so one project
   owns the workspace; (b) single-flight + save-storm debounce per corpus;
   (c) exporter nice/low-QoS gated on watcher-idle; (d) export-to-temp +
   atomic swap; (e) surface index posture (fresh/rebuilding/age) in
   symbols/preflight.
2. Re-run the continuity preflight against LittleMac once it's awake (peer
   FAIL vs WARN distinction).
3. Weekly fleet-report cron — only after a first report is reviewed.

## Decisions

- Distill guard: SKIP over merge, chosen deliberately — a merged frame
  stamped self-reported would let split-enforce authorize splits on
  mostly-distilled content; banked-frame staleness is already the hook age
  gate's job.
- Fleet-report cadence: skill-first (`/fleet-report`), cron only after one
  good report.
- The mesh reframes cross-machine propagation: peer daemons are directly
  probe-able over Tailscale, so continuity can be REMOTELY verified rather
  than trusting each teammate rebuilt.
- Red-honor linger window = 30 min.

## Invariants

- Binary skew is the silent failure mode: git carries the hooks/config to
  every machine, but NOT the Rust binaries the hooks call — a teammate on a
  stale binary gets silent wrong behavior, not an error.
- Never leave the persisted shell cwd off the repo root: the hook snapshot
  holds the old relative path and breaks before any command runs — use
  `(cd sovereign && …)` subshells.
- Transcript commit counts must dedupe by `tool_use` id.

## Dead ends

(None recorded — the cwd-drift episodes were papercuts fixed in-session, not
abandoned approaches.)

## Working set

`scripts/fleet-report.py` · `scripts/agent-preflight.py` ·
`sovereign/crates/sovereign-cli/src/session_cmd.rs` (distill guard) ·
`.claude/settings.json` · `.claude/skills/fleet-report/SKILL.md` ·
`SETUP.md` · `quality/agent-preflight.golden.json` ·
`sovereign/docs/specs/SESSION_CONTINUITY.md`

## Verification

Full suite 7948 / 0. Distill guard verified live against this session's own
banked frame (skip + provenance preserved). Four commits landed: `9941d71e`,
`73068537`, `b01e302b`, `323a5520`; tree clean.
