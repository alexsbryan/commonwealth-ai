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

One markdown file. YAML frontmatter + nine fixed `##` sections, in order.
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

Sections, each with its budget share (total ≤ 2,150 tokens — hard cap,
enforced by writers; a frame that cannot fit must drop detail, never sections):

| Section | Budget | Contract |
|---|---|---|
| `## Objective` | ~200 | The STANDING outcome the work serves, plus `Anchored in:` — the `ARCH_PRINCIPLES.md` sections its shape answers to. See §2.1. **Inherited, not re-authored.** |
| `## Goal` | ~50 | The task this session took on. Just the *what*; the *why* is `## Objective`. |
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

### 2.1 `## Objective` — the standing outcome (CONTRACT)

**The failure this section exists to prevent.** A frame is written by one
session, so every section naturally describes *that session*. The objective is
the one thing that is NOT session-scoped — it belongs to the initiative, which
outlives any frame. When it shared a slot with the per-session task, it lost.
Measured over the 67 frames banked on RuggedFox (2026-07-29):

- **21 of 63** non-empty frames stated their goal as a *delta from a previous
  frame* — "Item One's remaining half", "continue frame `d9935a7b`'s Next item
  2", "P4 DONE". The reason is mechanical, not sloppiness: a successor authors
  `Goal` fresh, and what is freshest in a successor's mind is the delta from the
  frame it just booted from. Each hop is a faithful summary of the last hop, and
  the chain drifts.
- Walk one lineage — `1b0e75f9` → `cde083ff` → `311ec4b7` → `8815fdb9` →
  `c96d55a6`, all on 2026-07-29. `311ec4b7` states the point plainly: "the wedge
  is *what can my hardware do BEFORE I commit* — a stranger's first run always
  says not measured." Two frames later the word *wedge* does not appear. The
  work continued; the reason for it did not survive.
- The same lineage recopied its own unglamorous backlog **verbatim** across
  three frames (the 43% spread, the `WorkerOverflow` capacity basis, retiring
  the block-split pin, the gossip stall). Nobody did them, nobody dropped them,
  nobody re-ranked them against the objective. That is the rut: a frame can
  carry a stale backlog forever at zero cost.

**Required content.** Four parts. The middle two are what make this section
anti-rut rather than merely anti-amnesia; the fourth is what makes it
anti-*drift*:

1. **The outcome**, stated as what a *user* gets when the initiative lands — not
   the increment, not the mechanism. Plus where it is specified: a doc path and
   section, or a plan path, so a successor can go deep on demand instead of by
   default.
2. **`Done when:`** — a falsifiable test at *initiative* altitude. Not "the
   gossip poller ships" but "a second node quotes a measurement it did not
   take." If you cannot write a test that could come back false, you do not yet
   have an objective; you have a direction.
3. **`Not worth continuing if:`** — the exit condition. The frames are already
   strong at falsification one altitude down (`## Dead ends`, and verdicts like
   "F10 is a DO-NOT-BUILD"). This applies the same discipline to the initiative,
   so abandoning it is a *legible outcome* rather than an admission.
4. **`Anchored in:`** — the numbered `ARCH_PRINCIPLES.md` sections this
   initiative's *shape* is accountable to, and where it knowingly deviates.
   One line; section numbers, not prose. Added 2026-08-02.

**Why the objective carries the architecture (2026-08-02).** A session boots
holding a frame and nothing else. Every one of the nine sections is episodic —
they describe an initiative's progress, never its constraints — so an
architectural commitment made in one session lands in `## Decisions`, reads to
the next successor as task trivia, and is dropped. Over a lineage the work
survives and the rules it was built under do not. That is the same mechanism
§2.1 already documents for the objective itself, one level down.

The anchor rides `## Objective` rather than a tenth section deliberately.
§2.2's finding is that unexamined recopying is the dominant frame pathology, and
a new standing section is one more thing to recopy without reading. `Objective`
is already the inherited, altitude-setting slot with a guard behind it, and a
principle that constrains the initiative's shape belongs at initiative altitude.
Same rule as the rest of the section: **inherited, not re-authored** — copy the
predecessor's `Anchored in:` verbatim, and if the work has moved under different
principles, say so in `## Decisions` rather than quietly re-deriving it.

Cheap to satisfy, and it is the *reading* that is the point: naming §4.1 means
opening §4.1, which is what a successor otherwise never does.

**Inheritance is the load-bearing rule.** When continuing a predecessor, COPY
its `## Objective` verbatim; edit it only when the objective genuinely changed,
and when it does, say so in `## Decisions`. Re-deriving it each session is the
exact mechanism that produced the drift above.

This is cheap to satisfy because of *when* it is asked. An agent's first frame
write happens while the boot-injected predecessor frame is still whole in its
context, so inheriting the objective is a copy — not research.

**Enforcement.** `upsert_frame` rejects any write touching `Goal`, `State`,
`Next` or `Decisions` while the frame's `Objective` would be blank
(`sovereign-tools/src/code/session_state.rs`). The check is on the post-write
body rather than on frame creation, so a legacy eight-section frame is asked
once too — the successors most at risk of ratholing are precisely the ones
resuming a long lineage. Distillation asks for it separately from `Goal` and is
instructed to answer `none stated` rather than infer one, since a fabricated
objective is worse than an absent one.

---

### 2.2 Lineage and carried items — the recopied backlog (CONTRACT)

§2.1 stops a lineage losing its *objective*. This stops it accumulating a
*backlog nobody owns*. Same audit, second finding: `311ec4b7` → `8815fdb9` →
`c96d55a6` recopied four `## Next` items verbatim across three frames — the 43%
spread, the `WorkerOverflow` capacity basis, retiring the block-split pin, the
gossip stall. None were done. None were dropped. None were re-ranked against the
objective. A frame can carry a stale backlog forever at zero cost, so it does.

**Frames form a chain.** Frontmatter gains an optional `predecessor: <session-id>`.
It is stamped by the writer from a `predecessor` sidecar file that
`sovereign session frames --claim-window` drops beside the incoming session's
frame directory at the boot hand-off — **the only moment both ids are known**,
since the window pointer still holds the outgoing session while the claim names
the incoming one. The sidecar is per-machine and prunes with the window
pointers; the frontmatter is durable, so once stamped a lineage stays walkable
offline and indefinitely.

The hand-off is a *file* rather than a function call because `sovereign-tools`
(the writer) cannot see `session_lineage` (in `sovereign-cli`) under the repo's
feature contract. Both sides treat the file as the contract: a bare session id,
no decoration.

**What the writer reports.** Every upsert walks up to 8 ancestors (cycle-safe)
and returns, on the write response:

| Field | Meaning |
|---|---|
| `carried[]` | `## Next` items **consecutive** ancestors were also carrying, with `depth` |
| `objective_sessions` | consecutive frames stating this same objective; 1 = fresh or changed |
| `advice` | present only when something is carried; names the count and the worst depth |

**Two surfaces, one computation.** The write response answers "am I recopying?"
The boot payload answers "what am I about to inherit?" — the donor frame's own
carried count, measured against *its* ancestors, delivered before the successor
picks up anything:

```
⟳ **3 of 4 `Next` items in this frame were already inherited** — the longest has
ridden 3 frames without being done or dropped. Re-rank against `## Objective`
before continuing it: do an item, drop it, or say why it stays.
```

It appears on `sovereign session frames` (human), in the `predecessor` object of
its `--json` (`carried_items`, `next_items`, `carried_worst_frames`,
`objective_sessions`, `inherited_advice`), and the boot hook emits
`inherited_advice` verbatim under the injected frame. Both surfaces call the
same combinators in `sovereign_contracts::frame`, so they cannot disagree — an
advisory that changes its mind between boot and write is worse than none.

**It stays silent for a healthy handoff.** Nothing carried and an objective
under 4 sessions old prints nothing at all. A signal that fires on every boot is
one agents learn to skip.

**Design rules, each of which is load-bearing:**

- **The write-side advisory rides the WRITE, not the boot.** At boot the
  successor has not written a `Next` yet, so there is nothing of its own to
  compare. At session end it is too late to act. The write is the one moment the
  author is holding the backlog.
- **CONSECUTIVE ancestors only.** An item that appeared, was dropped, and came
  back is a re-prioritisation — legitimate, and precisely the behaviour this
  feature wants to encourage. Flagging it would punish the cure.
- **Matching uses the overlap coefficient** (`|A∩B| / min(|A|,|B|)`), not
  Jaccard. The real failure mode is an item that gets *elaborated* as it is
  carried, not reworded: the block-split item grew from 6 content words to 18
  between frames, scoring 0.22 by Jaccard and 0.83 by overlap. Overlap asks "is
  one of these contained in the other", which is the actual question.
- **Biased toward under-reporting.** Items under 3 content words never match. A
  missed carry costs nothing; a false one teaches agents to ignore the signal.
- **Advisory, never blocking.** Carrying an item is often right. The contract is
  that carrying it must be a *decision* — do it, drop it, or say in
  `## Objective` why it stays.

Every failure path degrades silently: no `ps`, no sidecar, an unreadable
ancestor, a hand-edited cycle. Lineage sharpens the signal; it is never a
precondition for writing a frame.

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
"split soon" ≥250k, red "SPLIT" ≥500k — plus `frame ✓<age>` for this
session's frame freshness. The thresholds are deliberately ABSOLUTE, not
window-relative: the lever is cache-read cost (≈ avg_ctx × turns), which a
1M window does not change. Red ctx + fresh frame = split is safe right now.

**Threshold history, and why they moved twice.** Red went 140k → 250k on
2026-07-24 (operator call: the 140k line fired too early in practice), then
90k/250k → 250k/500k on 2026-08-02 (operator call: "all the mini frame
management hasn't earned the keep relative to the overhead"). Both moves
are the same correction. A split pays only when the cache-read it avoids
exceeds what it costs, and the cost is not zero: the donor writes a frame,
and the successor re-derives by hand everything 2,150 tokens could not
carry. §3a's counterfactual priced the *saving* at ~50% of session cost
and called it "nearly threshold-insensitive (46.5–51.4% across 100k–200k)"
— but that analysis never priced the *overhead*, so a threshold-insensitive
saving was read as a licence to split early and often. Below ~250k the
overhead dominates and the protocol was charging sessions for a benefit
they did not receive. The lever is real; it is a fat-context lever.

Storage: `~/.svrnmesh/sessions/<session_id>/frame.md` (single-writer per
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

1. **Statusline red `SPLIT` (ctx ≥500k)** → the operator (or the agent, when
   asked to wrap up) gets a frame written NOW, then forks (`/clear` or new
   session). Yellow (≥250k) means: write/refresh the frame at the next natural
   boundary. Below yellow the protocol is silent on purpose — see §3's
   threshold history for why splitting a thin session is a net loss.
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

### 3b. Which frame the successor gets (SHIPPED 2026-07-26)

With several workstreams live, "the newest frame" is the successor's only by
luck — and a wrong frame costs more than none (MEMORY_MODEL §5 E5 R1). So the
handoff is a **pointer first, frame second**:

| Surface | What it does |
|---|---|
| `sovereign session frames` | The index: one line per live frame — id, age, branch, status, provenance, `## Next` count, goal. In selection order. |
| `sovereign session frames <id>` | Dereference: print that frame whole. `<id>` is any unambiguous session-id prefix. |
| `sovereign session frames --json …` | Same, machine-readable, with every ranking signal per candidate. `--repo` / `--branch` / `--for-prompt` / `--limit` / `--max-age-days` scope it. |

- `session-boot.sh` (SessionStart) injects the **index**. It has no prompt to
  select against, so it does not try to select. A resumed session gets its
  OWN frame whole instead — that needs no selection.
- `inject-notes.sh` (first UserPromptSubmit) injects the **top-ranked frame
  whole**, once per session, recorded in
  `~/.svrnmesh/sessions/<id>/frame-inject.json`.
- Ranking is lexicographic: **branch match → prompt overlap → recency.**
  In-flight status is displayed but deliberately not ranked on — `status` is
  free text, and a `completed` frame is the normal good handoff.

Both surfaces are pure filesystem reads, so the handoff survives a dead daemon.

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

A second golden, `quality/session-frame.2fa2ddbb.golden.md`, is the E4a
reference (MEMORY_MODEL §5): hand-authored from `2fa2ddbb`'s transcript spine
to grade the encode-time write path independently. That session's
*self-reported* frame — written mid-work via `session_state`, no wrap-up
prompt — grades **78% weighted recall, zero hallucinated verification**
against it (Next 3/3, Invariants 2/3, weak on Decisions 2/4). This is the
artifact-level confirmation that a banked frame authorizes a split: the
strong path clears the bar without the successor ever paying for a
reconstruction.

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
