# The Memory Model — Context as Working Memory, the Brain as Long-Term Store

**Status:** Initiative compass, drafted 2026-07-24; first fleet cohort
re-measurement 2026-07-26 (§4a). Principles (§3) are the evaluation bar for
all memory-adjacent work; experiments (§5) are the roadmap. Per
`ARCH_PRINCIPLES.md §1.1`: §3 becomes contract as each experiment's build log
confirms it; until then treat any individual mapping claim as a hypothesis
with its measurement named.

**Owner context:** successor to (and generalization of) the bionic-suit
initiative. Everything remains accountable to `sovereign cache-audit`. Parent
specs: `SESSION_CONTINUITY.md` (the session frame = this model's index tier),
`cache-audit` (`--counterfactual`, `--ramp` = this model's measurement
instruments).

---

## 1. Thesis — we rediscovered the hippocampus for economic reasons

An LLM session's context window is working memory being abused as long-term
memory: append-only, no forgetting, and — unlike biological WM — charged
**rent on every token every turn** (cache-read ≈ avg_ctx × turns; measured at
76% of fleet session cost post-dedup, 2026-07-23). Humans pay ~zero rent on
LTM and a fixed cost on a tiny WM, and the resulting architecture is: reason
over **pointers and gists**, store content in vast external-to-attention
systems, reconstruct on demand, forget by predicted need.

Our optimization pressure is stronger than biology's (the rent is in
dollars *and* in long-context attention degradation), so we should expect the
same architecture to fall out — and empirically it has. The suit's components,
built purely from cost measurements, map onto the cognitive-science memory
model piece for piece (§2). This document makes the mapping explicit so it can
*generate predictions* instead of just flattering the design.

The one-sentence version a mid-level engineer can hold: **the context window
holds pointers and gists; the brain (notes, frames, facts, code graph) holds
everything else; retrieval is cheap, so eviction should be aggressive and
principled.**

---

## 2. The five stores — mapping table

| Cognitive system | Property | Our system | Status |
|---|---|---|---|
| Working memory (Cowan: ~4 chunks, chunks are pointers into LTM) | Tiny, expensive, where reasoning happens | Context window, target steady-state ≈ seed + active gist | Enforced only at split boundaries today; E3 is the gap |
| Hippocampal index (Teyler & DiScenna: stores pointers to cortical patterns, not content; recall = reinstatement) | Fast-written, small, per-episode | **Session frame** — spec §2's "pointers over prose" rule is the indexing property stated exactly | Shipped, split-safe (SESSION_CONTINUITY §3a) |
| Episodic LTM (specific experiences, fast write, decays) | One-shot encoding | Frames per session; transcript JSONL as the raw trace | Shipped |
| Semantic LTM (decontextualized regularities, slow consolidation) | Durable, generalized | Notes store (decision/invariant/attempt); commit harvest | Shipped |
| Neocortex / the environment itself (extended mind — Clark & Chalmers) | Vast, queryable, never "loaded" | Repo + SCIP graph + corpus: `symbols`/`callers`/`facts`/`code_search` | Shipped |

Consolidation (complementary learning systems — McClelland, McNaughton &
O'Reilly): episodic traces replay into the semantic store. Ours: notes written
at decision-time; commit harvest; the PreCompact distill hook is literally
*replay before the window is destroyed*. The §7 open question in
SESSION_CONTINUITY (should distill emit notes?) is the consolidation pathway
and should eventually ship.

---

## 3. Principles

**P1 — Pointers over prose, everywhere.** (Hippocampal indexing.) Any content
that exists in a queryable store enters working context as a pointer + gist,
never as a copy. Copies go stale and pay rent. Already the frame contract;
this principle extends it to *all* in-context state — tool results included
(P3).

**P2 — Write at encoding time; reconstruction has a ceiling.** (Generation
effect / levels-of-processing.) The agent holding the state writes
100%-fidelity traces; post-hoc distillation from the transcript measured 17%
recall. Traces written at the moment of the decision (notes) or the
transition (frame upserts) are the strong path; distillation is rescue
tooling and must never be load-bearing in a protocol. Consequence: invest in
encode-time tooling (E4a) before distill-prompt cleverness (E4b).

**P3 — Evict at work-item close.** (Fuzzy-trace theory: gist and verbatim are
parallel traces; verbatim is droppable once gist is banked.) When a work item
closes, its verbatim traces (file reads, tool output, build logs) are dead
weight — the note/commit/frame already banked the gist, and re-derivation via
code-intel is cheap. Splitting (SESSION_CONTINUITY §3a) is the coarse version;
E1 prices the fine-grained version; E3 builds it.

**P4 — Forget by need probability, not by TTL.** (Anderson & Schooler's
rational analysis: availability should track recency × frequency of use.) The
notes store's retention and the injection ranker should both run on measured
access statistics, not age. A note retrieved often and recently outranks a
newer never-retrieved one. E2.

**P5 — Dereference before load-bearing use.** (Confabulation control; DRM/
fuzzy-trace false memory is *gist-driven* — the more we run on gists, the more
plausible-but-false details the system will generate.) A pointer or gist
authorizes a query (`symbols`, `facts`, `notes`), never substitutes for one
when the answer feeds an edit, a commit, or a claim to the user. The grading
judge's hallucination penalty and the grounding gate are this principle in
existing clothes.

---

## 4. Evidence already banked

- **Gist-boot works at no measured quality cost:** successor session ramped on
  3,585 raw + 2,362 intel tokens, 0 repeat reads (cold baseline 10–55k, up to
  6 repeats) and immediately found a real bug in the donor's tooling
  (`a3e7e8bf`). SESSION_CONTINUITY §3a.
- **Rent dominates:** cache-read is 76% of actual fleet cost; splitting alone
  recovers 46.5–51.4%, nearly threshold-insensitive (H1).
- **Generation effect reproduced:** self-reported frame 100% recall vs
  distilled 17% (`svrn session grade`, golden-calibrated).
- **Interference relief reproduced:** the fresh-store successor caught the
  duplicate-usage bug the loaded donor session shipped past.
- **Batching is a minor lever** (H3 realizable ~4%): most serial small calls
  are stateful edit/build loops — genuinely sequential cognition, not waste.

---

## 4a. First fleet cohort read — 2026-07-26

Two days of fleet operation under the protocol (`session_state` + the
split-enforce hook both went live 2026-07-24). Cohorts from
`cache-audit --json`: **pre = 07-17..07-23 (n=23)**, **post = 07-25..07-26
(n=13)**. Reports: `~/.sovereign/reports/fleet-2026-07-24.md` (baseline) and
`fleet-2026-07-26.md`.

| median per session | pre | post | Δ |
|---|--:|--:|--:|
| cache-read tokens / turn (the rent line) | 258.0k | 159.8k | **−38%** |
| peak context | 466k | 257k | **−45%** |
| peak ctx / turn (accretion rate) | 2,659t | 2,088t | −21% |
| turns | 153 | 137 | −10% |
| code-intel calls | 2 | 6 | 3× |
| ramp raw (pre-first-Edit acquisition) | 18,636t | 17,388t | −7% |
| raw acquisition / turn | 340t | 352t | **flat** |
| cost | $57.81 | $15.92 | −72% (confounded) |

**Confirmed directionally.** Turns held roughly flat while peak context
halved — sessions accrete less per turn, which is the model's central claim.
Read token metrics, not dollars: the post cohort is mostly opus-5 against a
pre cohort of opus-4.8/fable-5, so the cost delta carries a pricing confound
the token deltas do not.

**P2 / E4a flipped the frame population.** Frame census: 23 frames, **18
self-reported / 5 distilled** — and every frame whose session started after
`session_state` shipped (07-24 04:30Z) is self-reported. The rescue path went
to zero in practice without a policy fight.

**The split hook is live and mostly obeyed.** Zero split events existed before
07-24; 38 events across 18 sessions since. 8 of 12 red-crossing sessions ended
within 30 min of first red (median post-red growth 10k). The 4 that ignored it
grew 7–43k further and lingered 3–13h — the failure mode is a live session
declining the directive, not the hook missing the crossing.

**Not moving — the retrieval half.** Ramp is flat (18.6k → 17.4k; gate passes
4/23 → 0/13) and raw acquisition per turn is flat (H4 held 10.1% → 10.3%).
Frames are being *written* but successors still re-acquire on boot: the §4
gist-boot evidence has not generalized past its two hand-run cases. The
savings so far come from the context ceiling (P3-coarse / splitting), not from
P1 pointers displacing reads. **This is the gap to attack next** — it is E4a's
mirror image on the read side, and it is now E5.

Caveats: post cohort is 2 days, n=13; windows overlap; one resumed long-lived
session sits in the post cohort by mtime with unchanged spend (medians absorb
it). Treat every number here as one window, not a trend.

---

## 5. Experiments

**E1 — Price eviction-at-close (H5 counterfactual). MEASURED 2026-07-24;
RE-MEASURED 2026-07-26 — follow-up prediction failing.**
H5 shipped in `cache-audit --counterfactual`: work-item close proxied by
`git commit` tool calls; on close, context returns to seed + ~1k gist per
closed item; the eviction re-prefills retained context once (5m cache write);
subsequent requests save the evicted cache-read.

Fleet result: **H5 = 5.8% ($40.71, 11 evictions) vs H1 ≈ 50%** — but the
per-session spread shows the constraint is *behavioral, not mechanical*:

- Sessions with zero in-transcript commits (verified: the two biggest,
  $62 and $176) score H5 = $0 — no boundaries, nothing to evict at.
- The one session practicing commit-per-work-item (34cf682b, 8 evictions):
  H5 = $29.27 = **72% of its H1** — without killing the session.
- Short sessions invert: 3fabc9ed H5 $0.85 > H1 $0.68 (a split's seed
  re-write doesn't amortize on short sessions; cheap evictions do).

Verdict: E3 is NOT justified as a splitting replacement on this number.
The durable findings instead: (1) **P3 is conditionally validated — its
value is gated on work-item closes existing in the transcript; commit
cadence is itself a memory-hygiene lever** (small, frequent commits create
eviction boundaries). (2) The dominant policy is **hybrid**: evict at close
where boundaries exist (5m write of ~50k retained, session survives), split
at threshold as the backstop for boundary-free growth. (3) Falsifiable
follow-up: as commit-per-item discipline spreads through the fleet
(CLAUDE.md protocol), H5's fleet share should climb toward the 34cf682b
ratio — re-measure in the weekly fleet report; if it doesn't move, boundary
density wasn't the binding constraint and this section is wrong.

**Re-measured 2026-07-26 — the prediction is failing, first window.** H5's
fleet share went **down**: 9.5% → **8.0%** ($147.53 → $109.68), evictions
34 → 29, fleet commits 47 → 38. In the post-adoption cohort (§4a) the
boundary density collapsed outright: **2 in-transcript commits across 13
sessions**. Commit-per-item discipline did not spread — the protocol text
landed, the behaviour did not.

Read carefully, this does not yet falsify P3's *mechanism* (34cf682b's 72%
still stands as the demonstration that boundaries pay when they exist); it
falsifies the *diffusion assumption* — that documenting the lever in
CLAUDE.md would produce boundaries. Standing interpretation until the next
window: **a memory lever that depends on model discipline does not
propagate** (the same finding E3's design constraint already asserts, now
with fleet evidence). If H5 is still flat or falling at the next
re-measurement, the honest move is to stop treating commit cadence as a
lever we can request and either (a) make work-item close a harness-detected
event rather than a git artifact, or (b) retire the H5 line from the ledger.

**E2 — Rational forgetting for the notes store.** Need-probability ranking
(recency × retrieval frequency) for injection and retention, replacing pure
relevance match + the punted TTL policy. Requires retrieval logging first —
measure before tuning. Gate: injection hit-rate (injected note actually used
by the session) improves against the current hook's baseline.

- **Retrieval logging + audit: SHIPPED 2026-07-24 (the measure-first half).**
  `inject-notes.sh` now appends one record per injection to
  `~/.sovereign/retrieval-log/<session>.jsonl` — the notes that entered
  context (id, kind, symbols, files, hook-extracted distinctive `terms`,
  retrieval frequency). `sovereign notes retrieval-audit` joins that log
  against the Claude Code transcript (pure local read, no daemon) and reports
  whether each injected note reappeared in the agent's OWN downstream actions
  (assistant text + tool-call inputs; tool *results* excluded). Two honest
  rates: **strong** = anchor(symbol|file) matches ÷ anchored notes; **any** =
  (anchor|content) matches ÷ all injected. The `injections` count per note is
  the recency×frequency raw signal the ranker will consume.
- **First baseline finding — `any%` saturates on-topic.** On a session
  working squarely on a note's topic, the note's distinctive terms flood the
  transcript and `any%` pegs at ~100% (measured: session 2fa2ddbb-lineage,
  20 notes, strong 75% / any 100%). A saturated metric can't show a ranker
  improving, so **`strong%` (anchor-based) is the gate metric** until the
  content signal is rarity/IDF-weighted (discount terms shared across the
  injected set — topic, not note-specific use). That calibration is itself
  deferred until fleet logs reveal the `any%` distribution — measure before
  tuning applies to the metric too. The tool prints this caveat inline when
  it detects saturation.
- **Fleet baseline established 2026-07-26 — `strong% = 32%`, and its
  denominator is inflated.** 15 sessions, **518 injections, 205 anchored,
  strong 32% / any 75%** (`sovereign notes retrieval-audit`). Per-session
  strong% ranges 0–52%. It replaces the single-session 75%, which was a
  working-squarely-on-topic outlier.
  - **Correction, same day (E5 R2):** `inject-notes.sh` printed all 8 ranked
    notes at full length — 15KB payloads — but the harness spills anything
    over ~10KB to a file and delivers a 2KB preview. The hook logged every
    note as injected regardless, so **the 32% is measured against a
    denominator containing notes that never entered context**. Roughly 13–20%
    of the payload was reaching the model; at the new 6000-char budget, 57%
    of ranked notes are delivered (measured over 3 real prompts).
  - Fixed by logging `delivered` per note and excluding dropped notes from
    every denominator. Pre-2026-07-26 rows are marked `legacy: delivery
    unknown` and **must not be compared against post-flag rows** — the tool
    prints this inline. The ranker's real gate is the first post-flag fleet
    number, not the 32%.
  - This also revises the saturation caveat above: `any%` pegged at ~100% on
    the 2fa2ddbb lineage, but at fleet scale it reads **75%** and is *not*
    saturated. Saturation is a property of on-topic sessions, not of the
    metric. `strong%` remains the gate (anchors are the falsifiable signal),
    but `any%` is now usable as a secondary read.
- **Remaining E2 work:** calibrate content matching (rarity/IDF weighting) →
  build the need-probability ranker → re-run the audit against the 32%
  baseline. The measure-first precondition is now satisfied: the logs exist.

**E3 — Live eviction mechanics.** Harness-side: on work-item close, replace
verbatim tool results with one-line gist + pointer. Blocked on E1's number.
Design constraint from the suit principle: hook/harness-enforced, never
model-discipline-dependent.

**E4 — Close the generation-effect gap (the 17% problem).** Three parts,
priority ordered by P2:
- **E4a (encode-time, strong path): TOOL SHIPPED 2026-07-24.** The
  `session_state` MCP tool (sovereign-tools `code/session_state.rs`,
  registered daemon + cli-dev, on `MCP_TOOLS_ALWAYS`) is a section-level
  frame upsert called at transitions, so the frame is *continuously*
  current and SessionEnd needs no LLM at all. Budget-gated (over-2k writes
  rejected with per-section counts), provenance always re-stamped
  self-reported, dogfooded on its own build session (838-token frame,
  8/8 sections). **Gate CLOSED 2026-07-24:** the self-reported frame of
  session `2fa2ddbb` (written mid-work via `session_state`, no wrap-up
  prompt) graded **78% weighted recall, zero hallucinated verification
  claims** against an independent golden hand-authored from that
  session's transcript spine (`quality/session-frame.2fa2ddbb.golden.md`;
  `svrn session grade`, exit 0). The successor-critical double-weighted
  sections carried (Next 3/3, Invariants 2/3); the weak spot was
  Decisions (2/4 — encode-time compression dropped two rationale
  bullets). P2 confirmed at the artifact level. **Adoption confirmed at the
  fleet level 2026-07-26:** 18 of 23 frames self-reported, and 100% of
  frames from sessions started after the tool shipped (§4a). E4a is the
  one lever in this document that propagated without discipline — because
  it is a tool call at a transition, not a habit to maintain. Contrast E1's
  diffusion failure; that contrast is the design lesson.
- **E4b (retrieval practice, weak path):** restructure the distill stage-2
  prompt from "summarize the spine" to "answer the eight section-questions,
  citing spine evidence per item" (testing effect / elaborative
  interrogation). Iterate against `svrn session grade`; baseline 17%.
- **E4c (richer encoding):** if E4b plateaus, the ceiling is the spine —
  enrich stage 1 (Edit outcomes, tool-call results summaries) before more
  prompt work. Prediction from P2: E4b+E4c together still land below E4a;
  measure to confirm, then set policy that distilled frames stay
  rescue-only regardless.

**E5 — Close the reconstruction gap (opened by the 2026-07-26 measurement).**
E1–E4 all address the *write* side; §4a shows the *read* side is where the
model is not paying out. Frames are written and injected, yet ramp is flat
(18.6k → 17.4k, 0/13 gate passes) and raw acquisition per turn is unmoved
(340t → 352t). Successors boot with a gist and then re-acquire the verbatim
anyway.

**Diagnosis 2026-07-26 — three mechanical faults, none of them discipline.**

- **R1 — the boot hook injects the newest frame, not the successor's.**
  `session-boot.sh` selected by `max(mtime)` over `~/.sovereign/sessions/*/frame.md`.
  With 4+ workstreams interleaved (24 live frames at time of writing) the
  newest frame is the right one only by luck. Direct evidence: session
  `40ab6490` was handed another thread's frame and spent its ramp hunting —
  `grep -rl -i "wrapped" ~/.sovereign/sessions/*/frame.md`, then reading
  three frames by hand. Natural experiment (n=2): right-frame `86060bbd`
  ramped 16 calls / 9.3k; wrong-frame `40ab6490` ramped 27 calls / 20.9k.
- **R2 — the boot payload exceeded the harness inline cap and spilled.**
  Claude Code persists any hook output over ~10KB (smallest observed spill
  9.8KB across 80 transcripts) to a file and shows a 2KB preview. The boot
  brief ran 11.4KB, so the first tool call of a booted session was literally
  `Read .../tool-results/hook-<uuid>-stdout.txt` — the budgeted brief
  converting itself into an unbudgeted raw read, inside the very metric it
  exists to shrink.
- **R3 — no dereference surface for frames.** `sovereign session` has
  list/distill/grade and `session_state` is upsert-only, so "find the right
  frame" costs greps and Reads. P1 says a frame should enter context as a
  pointer; there was nothing to dereference with.

**Phase 0 (instrument) + Phase 1 (stop the spill): SHIPPED 2026-07-26.**

- `session-boot.sh` writes `~/.sovereign/sessions/<id>/boot.json` — chosen
  frame, its age/provenance, `frame_candidates`, `frame_is_own`, payload
  chars. Before this, *which frame a session received was unrecoverable*, so
  no honest classifier could exist.
- Both hooks are now payload-budgeted (boot 8000 chars, notes 6000) with
  overflow degraded to a dereferenceable pointer. Measured after: boot
  payload 11.4KB → 7.7KB, under the spill floor.
- `cache-audit --ramp --classify` buckets ramp raw acquisition into
  **boot-spill / frame-hunt / frame-covered / new-task**, reading boot.json
  for ground truth and printing `UNKNOWN` rather than guessing when it is
  absent. First run over the 5 most recent sessions (all pre-provenance, so
  frame-covered is unmeasurable): **boot-spill 10,303t and frame-hunt 6,314t
  out of 53,918t ramp raw — 31% of ramp was the two mechanical wastes**, and
  5,872t of the frame-hunt was `40ab6490` alone, confirming the hand read.

**Phase 2 (frame selection + dereference): SHIPPED 2026-07-26.** R3 is closed
and R1's selector is gone.

- **`sovereign session frames`** — the index: one line per live frame
  (id, age, branch, status, provenance, next-count, goal), in selection
  order. **`sovereign session frames <id>`** — the dereference verb, prints
  one frame whole. Both are pure filesystem reads over
  `~/.sovereign/sessions/*/frame.md`, so they work with the daemon down.
- **`session-boot.sh` injects the index, not a frame** (~1.4KB / ~350 tokens
  for 8 entries, against ~4.5KB for one possibly-wrong frame). The single
  exception is a resume/compact of a session's OWN frame, which is injected
  whole — no selection is involved there, so none can be wrong.
- **Full-frame injection moved to the first `UserPromptSubmit`**
  (`inject-notes.sh`), where the prompt exists. Once per session, marked by
  `~/.sovereign/sessions/<id>/frame-inject.json`, which records the chosen
  frame, the candidate count, and every ranking signal — the provenance
  `--ramp --classify` needs to grade selection. The frame and notes payloads
  share one budget on that turn (frame ≤4500, notes ≤3200) so turn one cannot
  re-create the Phase 1 spill.

**Ranking is lexicographic: branch match → prompt overlap → recency.** No
weights; every signal is emitted in `--json` whether it was used or not, so
"would a different order have picked a different frame?" stays answerable
against real sessions (E2's measure-before-tuning rule).

**Correction to this section's own sketch, made against the live store.** The
plan said "in-flight + branch + recency". In-flight is now *recorded but not
ranked on*, for two reasons visible the moment the index ran over the real 23
frames: (1) `status` is free text — the store carries `in-flight`,
`completed`, AND `work-complete-uncommitted` — so a predicate over it is a
guess about a string; (2) sorting in-flight first buried the frame the
successor actually needed. The session that built Phase 2 was handed a
`completed` frame whose `## Next` *was* the entire task; ranked below every
in-flight frame it fell past the 8-line cut and would not have been shown at
all. A completed frame is the normal good handoff — completion is what let
the donor write down what comes next.

Gate (unchanged, still to be measured): median ramp raw for frame-booted
successors below 10k with no regression in successor error rate. Design
constraint inherited from E1's diffusion failure: whatever lands must be
harness- or tool-shaped, not a protocol paragraph asking agents to read less.

---

## 6. ROI ledger and the regime-change prediction

Banked: ~50% (H1 splitting, live as standing protocol). Candidate: E1/E3
eviction — upper bound to be measured; back-of-envelope, holding a ~150k-avg
session to ~50–60k steady state cuts the 76% rent line by roughly
two-thirds, overlapping heavily with H1 (the levers substitute more than
they stack on long sessions; E1 quantifies the residual).

**Observed 2026-07-26 (§4a):** rent per turn fell 38% and median peak context
fell 45% in the first post-adoption cohort — consistent with the banked H1
figure being realized in practice rather than only in counterfactual. The
counterfactual levers moved accordingly: H1@100k 49.0% → 45.0% (less headroom
left to recover, which is what success looks like on a counterfactual),
H5 9.5% → 8.0% (§5 E1), H4 10.1% → 10.3% (unmoved).

**Prediction worth watching (falsifiable):** in the current regime the turn-0
preamble (~43k) is a 5% lever (H2a). In a gist-first steady state near seed,
the preamble becomes the *dominant* remaining rent, and the lever ordering
inverts — preamble diet and boot-brief tiering become first-class. If E1/E3
land and H2a's share does NOT grow in the next fleet counterfactual, this
model is missing something.

- **First read 2026-07-26: consistent, too small to call.** H2a share
  **5.3% → 6.1%** (+0.8pp) on a preamble that itself barely moved (46k →
  45k) — i.e. the share grew because the denominator shrank, which is the
  predicted mechanism. One window, sub-point-size effect; do not bank it.
  Keep `preamble_avg_ktok` in the fleet report as the tracker.

Costs honestly on the other side: encode-time writes (~2k tokens/frame + note
calls), reconstruction failures (caught by the `--ramp` gate), eviction
re-prefill (priced in H5), infra maintenance, and P5 discipline against
gist-confabulation.

---

## 7. Non-goals and risks

- **Not building:** vector-DB-of-everything, whole-transcript RAG, or any
  "load more context" path. The model says load *less*, better-indexed.
- **Risk — gist confabulation (P5):** more gist-reasoning means more
  plausible-but-false detail generation. Mitigation is cultural + structural:
  dereference-before-use, hallucination-penalized grading, grounding-gate
  posture. If successor error rates rise as eviction gets aggressive, this is
  the first suspect.
- **Risk — the mapping seducing us.** Cog-sci is the compass, cache-audit is
  the terrain. Every E-item carries a measured gate; when the analogy and the
  measurement disagree, the measurement wins.
