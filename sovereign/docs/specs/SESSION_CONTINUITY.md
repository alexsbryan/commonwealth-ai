# Session Continuity — the Session Frame, Zero-Friction Boot, and `session distill`

**Status:** Schema v1 DEFINED + golden + grader shipped; ALL THREE write
paths and boot integration SHIPPED (paths 2–3 + boot 2026-07-23; path 1's
`session_state` tool 2026-07-24). Split experiment PASSED (§3a) —
**red-SPLIT is the standing protocol**, conditioned on a fresh self-reported
frame. Per `ARCH_PRINCIPLES.md §1.1`: §2 (schema), §3a (split protocol), and
§5 (grading) are contracts.

**Owner context:** part of the agent-efficiency ("bionic suit") initiative —
see `sovereign cache-audit`, `scripts/agent-preflight.py`, and the `facts` MCP
tool. The measured problem: 10 audited sessions, 190k–379k raw-read
tokens each, zero code-intel calls (the original $8.7k figure predates the
`fresh_usage` message.id dedup, `a3e7e8bf` — pre-dedup dollar totals are
~2.5x inflated; ratios survive); and every session that dies at ~500k–1M
context takes its task state with it, forcing hand-written wrap-up messages
(the FactStore session literally booted from one pasted into turn 1).

---

## 1. The thesis — make the context window disposable

A 500k-token session's *essential* context is 1–2k tokens: goal, position,
next actions, decisions, invariants, dead ends, working set, verification
state. Everything else is re-derivable — cheaply, now that the code-intel
layer (`symbols`/`callers`/`facts`/`code_search`) answers pointer-shaped
questions in one round-trip. So continuity is not "transfer the window"; it is
"externalize the essential state continuously, and make boot assemble it."

The unit of externalized state is the **session frame**: one small, living
markdown document per session, upserted as work progresses (or distilled after
the fact), stored in the brain, gossiped over the mesh like notes. A "session
token" is then just a key: `svrn://session/<node_id>/<session_id>` resolves to
a frame any harness can inject at boot.

Division of labor with the notes store: **notes are the append-only "what we
learned" log** (decision/invariant/attempt survive the session and serve
*every* future session); **the frame is the mutable "where are we" pointer**
(position + next actions, valuable precisely because it is current). The frame
*references* note ids; it never duplicates their content.

---

## 2. The frame schema (v1) — CONTRACT

One markdown file. YAML frontmatter + eight fixed `##` sections, in order.
Machine-checkable: a validator can assert frontmatter keys and section
presence; a grader can score section-by-section (§5).

```yaml
---
schema: session-frame/v1
session_id: <uuid>              # the harness's session/transcript id
harness: claude-code            # claude-code | cursor | sdk | unknown
model: <model-id>
repo: commonwealth-ai
branch: main
head_at_end: <short-sha>        # git HEAD when the frame was last written
started_at: <ISO8601>
ended_at: <ISO8601 | null>      # null while in-flight
status: in-flight | completed | abandoned
provenance: hand-written | self-reported | distilled
notes: [<note-id>, ...]         # durable notes written during the session
---
```

Sections, each with its budget share (total ≤ 2,000 tokens — hard cap,
enforced by writers; a frame that cannot fit must drop detail, never sections):

| Section | Budget | Contract |
|---|---|---|
| `## Goal` | ~100 | The task AND the standing objective it serves. A successor must know *why*, not just *what*. |
| `## State` | ~400 | Done (with proof — test counts, live verification), in-flight, not-started. Facts only; no narrative. |
| `## Next` | ~250 | Ranked, concrete actions with `file:line`/symbol anchors. The single highest-value section for a successor. |
| `## Decisions` | ~350 | Choice + the *why* + note-id pointer where one exists. |
| `## Invariants` | ~350 | Gotchas that will bite a fresh session. The FactStore session's "read cargo.exit, not the wrapper exit" class. |
| `## Dead ends` | ~150 | Approaches tried and abandoned, so successors don't repeat them. |
| `## Working set` | ~200 | Files + key symbols touched — *pointers*, never content. The successor re-derives via `symbols`/`facts`. |
| `## Verification` | ~200 | Suite result at last write, deploy state (daemon binary/pid), preflight status, uncommitted files. |

Rules that keep frames honest:

- **Pointers over prose.** Name a symbol, cite `file:line`, reference a note
  id. The code-intel layer makes pointers cheap to expand; pasted content goes
  stale and bloats the budget.
- **Freshness is part of the artifact.** `head_at_end` lets any consumer diff
  the frame against `recent_changes` and warn: "frame is N commits behind —
  treat `## Next` skeptically." Same discipline as the `facts` tool's
  `lags_graph`.
- **Verification claims carry their evidence** ("7928 pass / 0 fail,
  cargo exit 0"), mirroring the report-actual-metrics convention.

---

## 3. Write paths (paths 2–3 SHIPPED 2026-07-23; path 1 proposal)

Three, in trust order — the frame must exist even when the agent never
cooperated:

1. **Self-report** — SHIPPED 2026-07-24 (MEMORY_MODEL.md E4a): the
   `session_state` MCP tool (also `svrn tools call session_state`) is a
   section-level upsert of `frame.md` called at transitions (task start,
   plan step done, blocker hit). Provided sections replace their bodies,
   others are preserved; every write re-stamps `provenance: self-reported`
   (an encode-time write upgrades a distilled frame); over-budget writes
   are REJECTED with per-section token counts. Cheapest and sharpest;
   discipline-dependent, so never the only path — paths 2–3 remain the
   suit-enforced floor.
2. **Harness lifecycle hooks** — SHIPPED: `.claude/hooks/session-frame.sh`
   wired on `PreCompact` (snapshot before the window is destroyed) and
   `SessionEnd` (final flush). The hook exits in <100ms and runs distill
   fully detached — safe because the transcript JSONL retains full history
   regardless of compaction, so a frame written a minute late is still
   correct. Skips transcripts <100KB (no successor value); per-session
   lockfile (10-min TTL) dedups PreCompact/SessionEnd firing together;
   falls back to `--no-llm` spine when the daemon is down. Verified by
   distilling the session that built it (valid 8-section frame).
   The suit principle: never rely on model discipline for anything
   load-bearing.
3. **Retrospective distillation** — SHIPPED: `sovereign session distill
   <session-id>` (§4). Rescues past sessions and any harness that only
   leaves a transcript.

**Splitting signal (SHIPPED 2026-07-23):** the statusline
(`.claude/scripts/read-budget-statusline.py`) renders `ctx <N>k` from the
last assistant `usage` record (actual context, not a heuristic) — yellow
"split soon" ≥90k, red "SPLIT" ≥250k (raised from 140k on 2026-07-24;
operator call — the 140k line fired too early in practice) — plus
`frame ✓<age>` for this
session's frame freshness. The thresholds are deliberately ABSOLUTE, not
window-relative: the lever is cache-read cost (≈ avg_ctx × turns), which a
1M window does not change. Red ctx + fresh frame = split is safe right now.

Storage: `~/.sovereign/sessions/<session_id>/frame.md` (single-writer per
session; last-write-wins upsert). Mesh gossip + privacy model follow the work
atlas (`node.default_privacy`; private frames structurally never gossip).

---

## 3a. Split protocol — STANDING (CONTRACT, 2026-07-23)

The live experiment passed: a successor session (`3fabc9ed`) booted from a
self-reported frame and worked a real backlog item cold — ramp **3,585 raw +
2,362 intel tokens, 0 repeated reads** (gate ≤5k/0; cold baseline 10–55k with
up to 6 repeats), first edit at request 6/38, zero re-reads of docs the frame
summarized, zero user re-explanation. Quality cost: none measurable — the
successor's first work stretch found and fixed a real bug in the donor's own
tooling (`a3e7e8bf`). Counterfactual pricing puts the split habit at ~50% of
session cost, nearly threshold-insensitive (46.5–51.4% across 100k–200k).

The protocol:

1. **Statusline red `SPLIT` (ctx ≥250k)** → the operator (or the agent, when
   asked to wrap up) gets a frame written NOW, then forks (`/clear` or new
   session). Yellow (≥90k) means: write/refresh the frame at the next natural
   boundary.
2. **Only self-reported or hand-written frames authorize a split.** The agent
   holding the state writes 100%-fidelity frames; distilled frames (88%
   recall as of stage-2 v4, up from 17% — see §5) exist to rescue sessions
   that died uncooperatively — never split *onto* one. The rule is about
   provenance, not just score: a distilled frame is a reconstruction, and
   the encode-time write is always the strong path (MEMORY_MODEL P2). If the
   freshest frame is `provenance: distilled`, refresh it via self-report
   before forking.
3. **The successor works from the frame + code-intel** (`symbols`/`callers`/
   `facts`/`notes`), does not re-read `SYSTEM_OVERVIEW`/specs the frame
   already summarizes, and self-measures after its first work stretch:
   `sovereign cache-audit --ramp --session <id>` — gate: ≤5k raw tokens,
   0 repeated reads. A FAIL is reported with the section of the frame that
   failed to carry, and feeds §5 iteration.

---

## 4. `sovereign session distill` (build in flight)

Two stages, deliberately separable:

**Stage 1 — deterministic spine extraction** (no LLM). Parse the transcript
JSONL (same source `cache-audit` reads). Keep: real user turns (drop
tool-result carriers, hook payloads, `<system-reminder>`/local-command
wrappers), assistant text blocks, Edit/Write file paths, tool-call counts,
timestamps/model. Measured on the golden's source session: 4.1MB → ~40k chars
(~1% of transcript). User turns are the highest-signal tokens in the
transcript — they carry every goal statement and steer.

**Stage 2 — frame synthesis** (LLM, v2 2026-07-24: retrieval practice per
MEMORY_MODEL E4b). One local-daemon chat call *per body section*, not one
call for the whole frame: each call carries the spine plus one focused
question ("what operational traps remain true for a successor?"), and every
answer bullet must cite the spine item(s) it came from (`[u3]`, `[a17]` —
items are numbered in the spine render). Citations are machine-enforced:
`parse_cited_bullets` drops uncited bullets (prose instructions alone don't
hold on local models — same lesson as the grade judge's contradiction
citations), and a single re-ask recovers the occasional call where the model
ignores the citation rule entirely. The *mined* sections (Next, Decisions,
Invariants, Dead ends) additionally sweep the FULL spine in chunks with
stable global item ids, answers unioned + deduped — their golden content
lives in mid-session debugging, which the ends-biased window trim made
invisible (that trim was the single biggest cause of the 17% v1 baseline).
Goal/State/Verification stay on the ends-biased fitted spine. Validate
frontmatter + sections, stamp `provenance: distilled`. `--no-llm` stops after
stage 1 and emits the spine (still useful raw; also the daemon-down fallback —
degrade honestly, never silently).

Precedence: distill never overwrites a `provenance: self-reported` frame — it
refreshes `spine.txt` and skips the frame write (`--force` overrides, e.g. for
grading runs). The encode-time write is the strong path (§3 rule 2); before
this guard, the SessionEnd distill was restamping banked frames as `distilled`,
silently demoting them to rescue-only in split-enforce. The reverse direction
already held: a `session_state` upsert over a distilled frame upgrades
provenance to `self-reported`.

Grading loop: distill the golden's source session, score against the golden
(§5), iterate on the stage-2 prompt. The intent-forced-prompt lesson from
`CODE_INTEL_CHAT.md §3.2` applies: the prompt is the lever; expect to tune it
against the golden, not trust the first draft. (The v1→v2 iteration bore this
out: 17% → 29% from retrieval-practice questions alone → 88% once the mined
sections swept the full spine.)

Known tradeoff: the union sweep makes distilled frames rich but fat
(measured ~3.9k tokens vs the ~2k self-reported budget). Acceptable for a
dead-session rescue artifact; E4c candidate: judge-based near-dup merge
across chunk answers.

---

## 5. Golden + grading — CONTRACT

`quality/session-frame.golden.md` is the reference frame for session
`e09c5e3d` (FactStore/watcher-revival), hand-written from the full transcript.
Versioned like `agent-preflight.golden.json`: when the schema or the grading
bar moves, the golden moves in the same PR.

Grading a distilled frame against a golden is per-section recall of
*load-bearing items* (each golden section's bullet is one item; a distilled
frame scores by how many it captures, judged leniently on wording, strictly on
facts — a wrong sha, count, or symbol name scores zero for that item):

- `## Next` and `## Invariants` are weighted double — they are what a
  successor acts on first and what protects it from repeating pain.
- Hallucinated items (present in the distilled frame, contradicted by the
  transcript) are −1 each: a frame that invents state is worse than a sparse
  one. Same epistemic posture as the grounding gate.

Pass bar for promoting a stage-2 prompt: ≥70% weighted recall, zero
hallucinated verification claims.

**Grader SHIPPED 2026-07-23:** `svrn session grade <id|path> [--golden <path>]`
— one daemon judge call per golden section (deterministic everything else:
bullet extraction, weighting, arithmetic, pass bar; exit 0/1/2). Judge
calibration lesson: the local model flags candidate detail merely ABSENT
from the golden as hallucination despite prose instructions not to, so the
judge must cite the reference item each hallucination CONTRADICTS and the
CLI drops entries that cannot (or that cite an item the judge itself marked
captured). Self-calibration: golden-vs-golden grades 42/42 = 100% PASS.

**Stage-2 iteration history (all graded on e09c5e3d vs the golden):**
- v1 single-shot "write all eight sections": **17% FAIL** — Next 0/4,
  Invariants 0/9; the window trim hid mid-session content and one call
  spread over eight sections summarized at wrap-up altitude.
- v2 retrieval practice (per-section questions, machine-enforced spine
  citations): **29% FAIL** — Decisions 3/5→5/5 proved the mechanism, but
  Invariants stayed 0/9: still invisible behind the trim.
- v3/v4 + chunked full-spine sweep for mined sections + uncited re-ask:
  **88% PASS** — Goal 1/1, State 6/7, Next 3/4, Decisions 5/5,
  Invariants 9/9, Dead ends 0/2, Verification 1/1, zero hallucination
  penalties. Residual: Dead ends items are one-line mid-session asides and
  flicker 0–1/2 across runs.

Distilled-frame recall is now rescue-grade rather than token-grade; the
§3a rule stands — only self-reported/hand-written frames authorize a split,
distilled frames rescue sessions that died uncooperatively.

---

## 6. Boot integration (SHIPPED 2026-07-23 — the zero-friction payoff)

The frame is Tier 2 of the boot brief (`sovereign code brief` grown into the
assembler): Tier 0 = brain health + watcher/index freshness + atlas overlaps
(~200 tokens, always); Tier 1 = task-relevant notes + conventions + drift
headlines (~800, conditioned on the prompt); Tier 2 = the session frame
(the newest frame for this repo, injected automatically). Delivered as the
`session-boot.sh` SessionStart hook for Claude Code and the `briefing` MCP
tool for every other harness — one assembler, one budget, every consumer.
Replaces the read-two-docs-plus-seven-calls session-start ritual; the §3a
experiment ran over exactly this path (frame injected at boot, no pasting).

## 7. Open questions

- Multi-writer frames (two agents on one shared session token) — punt: v1 is
  single-writer, last-write-wins; the atlas already surfaces the collision.
- Should `distill` also *emit* notes (decision/invariant candidates found in
  the spine but absent from the store)? Likely yes, behind a flag, as
  suggestions — auto-writing notes from an LLM pass risks polluting the store.
- Frame retention: frames are per-session and pile up; likely fold into the
  notes-store retention policy once gossip lands.
