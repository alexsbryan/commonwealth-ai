# Session Continuity — the Session Frame, Zero-Friction Boot, and `session distill`

**Status:** Schema v1 DEFINED + first golden frame hand-written (2026-07-23,
from session `e09c5e3d` — the FactStore/watcher-revival marathon). `sovereign
session distill` is the build in flight; write-path hooks and boot integration
are planned. Per `ARCH_PRINCIPLES.md §1.1`: §2 (schema) and §5 (grading) are
contracts once the golden lands; §3–§4 are proposals until their build logs say
otherwise.

**Owner context:** part of the agent-efficiency ("bionic suit") initiative —
see `sovereign cache-audit`, `scripts/agent-preflight.py`, and the `facts` MCP
tool. The measured problem: 10 audited sessions, $8.7k, 190k–379k raw-read
tokens each, zero code-intel calls; and every session that dies at ~500k–1M
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

1. **Self-report** — a `session_state` upsert call at transitions (task start,
   plan step done, blocker hit). Cheapest and sharpest; discipline-dependent,
   so never the only path. (Proposal.)
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
"split soon" ≥90k, red "SPLIT" ≥140k — plus `frame ✓<age>` for this
session's frame freshness. The thresholds are deliberately ABSOLUTE, not
window-relative: the lever is cache-read cost (≈ avg_ctx × turns), which a
1M window does not change. Red ctx + fresh frame = split is safe right now.

Storage: `~/.sovereign/sessions/<session_id>/frame.md` (single-writer per
session; last-write-wins upsert). Mesh gossip + privacy model follow the work
atlas (`node.default_privacy`; private frames structurally never gossip).

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

**Stage 2 — frame synthesis** (LLM). One local-daemon chat call
(`POST :9741/v1/chat/completions`): spine in, schema-v1 frame out; validate
frontmatter + sections, stamp `provenance: distilled`. `--no-llm` stops after
stage 1 and emits the spine (still useful raw; also the daemon-down fallback —
degrade honestly, never silently).

Grading loop: distill the golden's source session, score against the golden
(§5), iterate on the stage-2 prompt. The intent-forced-prompt lesson from
`CODE_INTEL_CHAT.md §3.2` applies: the prompt is the lever; expect to tune it
against the golden, not trust the first draft.

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

---

## 6. Boot integration (proposal — the zero-friction payoff)

The frame is Tier 2 of the boot brief (`sovereign code brief` grown into the
assembler): Tier 0 = brain health + watcher/index freshness + atlas overlaps
(~200 tokens, always); Tier 1 = task-relevant notes + conventions + drift
headlines (~800, conditioned on the prompt); Tier 2 = the session frame
(conditioned on a token/claim match). Delivered as a SessionStart hook for
Claude Code and a `briefing` MCP tool for every other harness — one assembler,
one budget, every consumer. Replaces the read-two-docs-plus-seven-calls
session-start ritual.

## 7. Open questions

- Multi-writer frames (two agents on one shared session token) — punt: v1 is
  single-writer, last-write-wins; the atlas already surfaces the collision.
- Should `distill` also *emit* notes (decision/invariant candidates found in
  the spine but absent from the store)? Likely yes, behind a flag, as
  suggestions — auto-writing notes from an LLM pass risks polluting the store.
- Frame retention: frames are per-session and pile up; likely fold into the
  notes-store retention policy once gossip lands.
